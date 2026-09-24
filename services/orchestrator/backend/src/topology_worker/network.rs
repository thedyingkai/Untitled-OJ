//! Background network responsibilities.
use crate::topology_worker::context::{bounded_detail, now_ms};
use crate::topology_worker::observation::runtime_health;
use orchestrator_legacy::ApiBinding;
use orchestrator_legacy::ApiBindingState;
use orchestrator_legacy::TopologyDrift;
use orchestrator_legacy::TopologyDriftKind;
use orchestrator_legacy::TopologyEndpointStatus;
use orchestrator_legacy::TopologyHealth;
use orchestrator_legacy::TopologyLinkStatus;
use orchestrator_legacy::TopologyResourceKind;
use orchestrator_legacy::TopologySpec;
use orchestrator_legacy::TopologyStatus;
use orchestrator_legacy::parse_endpoint_id;
use orchestrator_legacy::validate_endpoint_id;
use orchestrator_runtime::RuntimeDesiredState;
use orchestrator_runtime::RuntimeObservedState;
use orchestrator_storage::StoredRuntimeInstance;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::Read;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

pub(super) const NETWORK_PROBE_TIMEOUT: Duration = Duration::from_millis(750);

pub(super) const NETWORK_PROBE_CONCURRENCY: usize = 16;

pub(super) const ENDPOINT_PROBE_BATCH: usize = 512;

pub(super) const LINK_PROBE_BATCH: usize = 1_024;

pub(super) const NETWORK_OBSERVATION_MAX_AGE_MS: i64 = 120_000;

pub(super) const NETWORK_RESPONSE_LIMIT: usize = 4_096;

pub(super) const ENDPOINT_EVIDENCE_PREFIX: &str = "network probe:";

pub(super) const LINK_EVIDENCE_PREFIX: &str = "source probe:";

#[derive(Debug, Clone)]
pub(super) struct EndpointProbeTask {
    pub(super) endpoint: String,
    pub(super) service_id: String,
    pub(super) protocol: String,
    pub(super) health_path: String,
}

#[derive(Debug, Clone)]
pub(super) struct LinkProbeTask {
    pub(super) source_endpoint: String,
    pub(super) source_service_id: String,
    pub(super) source_protocol: String,
    pub(super) target_endpoint: String,
    pub(super) target_service_id: String,
}

pub(super) struct NetworkObservationContext<'a> {
    pub(super) api_bindings: &'a [ApiBinding],
    pub(super) link_probe_source_endpoints: &'a BTreeSet<String>,
    pub(super) previous_status: Option<&'a TopologyStatus>,
    pub(super) network_probes: &'a NetworkProbePool,
    pub(super) observed_at: &'a str,
}

pub(super) fn observed_network_status(
    spec: &TopologySpec,
    relevant: &[&StoredRuntimeInstance],
    context: NetworkObservationContext<'_>,
    drift: &mut Vec<TopologyDrift>,
) -> (Vec<TopologyEndpointStatus>, Vec<TopologyLinkStatus>) {
    let NetworkObservationContext {
        api_bindings,
        link_probe_source_endpoints,
        previous_status,
        network_probes,
        observed_at,
    } = context;
    let now = now_ms();
    let previous_endpoints = previous_status
        .map(|status| {
            status
                .endpoints
                .iter()
                .map(|endpoint| (endpoint.endpoint.as_str(), endpoint))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let mut endpoint_tasks = Vec::new();
    let mut endpoint_statuses = BTreeMap::new();
    let binding_consumers = spec
        .links
        .iter()
        .filter(|link| link.enabled && !link.api_bindings.is_empty())
        .map(|link| link.source_endpoint.as_str())
        .collect::<BTreeSet<_>>();
    for endpoint in &spec.endpoints {
        let configured_deployment = endpoint
            .config
            .as_object()
            .and_then(|config| config.get("deployment_id"))
            .and_then(Value::as_str)
            .filter(|deployment_id| !deployment_id.trim().is_empty());
        let matching = relevant
            .iter()
            .copied()
            .filter(|stored| {
                stored.instance.service_id == endpoint.service_id
                    && configured_deployment.map_or_else(
                        || stored.endpoint == endpoint.endpoint,
                        |deployment_id| stored.instance.deployment_id == deployment_id,
                    )
            })
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            endpoint_statuses.insert(
                endpoint.endpoint.clone(),
                TopologyEndpointStatus {
                    endpoint: endpoint.endpoint.clone(),
                    health: TopologyHealth::Unknown,
                    reachable: false,
                    latency_ms: None,
                    message: if matching.is_empty() {
                        "no runtime projection owns this exact endpoint".to_string()
                    } else {
                        "multiple runtime projections claim this exact endpoint".to_string()
                    },
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        let stored = matching[0];
        if stored.instance.desired_state != RuntimeDesiredState::Running
            || stored.instance.observed_state != RuntimeObservedState::Running
            || runtime_health(&stored.instance.health) != TopologyHealth::Healthy
        {
            endpoint_statuses.insert(
                endpoint.endpoint.clone(),
                TopologyEndpointStatus {
                    endpoint: endpoint.endpoint.clone(),
                    health: if runtime_health(&stored.instance.health) == TopologyHealth::Unhealthy
                    {
                        TopologyHealth::Unhealthy
                    } else {
                        TopologyHealth::Unknown
                    },
                    reachable: false,
                    latency_ms: None,
                    message: "exact runtime projection is not healthy and Running".to_string(),
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        if binding_consumers.contains(endpoint.endpoint.as_str()) {
            endpoint_statuses.insert(
                endpoint.endpoint.clone(),
                TopologyEndpointStatus {
                    endpoint: endpoint.endpoint.clone(),
                    health: TopologyHealth::Healthy,
                    reachable: true,
                    latency_ms: None,
                    message: "outbound ApiBinding consumer health is derived from its exact RuntimeInstance"
                        .to_string(),
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        endpoint_tasks.push(EndpointProbeTask {
            endpoint: endpoint.endpoint.clone(),
            service_id: endpoint.service_id.clone(),
            protocol: endpoint.protocol.clone(),
            health_path: if endpoint.health_path.is_empty() {
                "/health".to_string()
            } else {
                endpoint.health_path.clone()
            },
        });
    }
    endpoint_tasks.sort_by_key(|task| {
        previous_endpoints
            .get(task.endpoint.as_str())
            .and_then(|status| {
                trusted_observation_ms(
                    status.observed_at.as_str(),
                    &status.message,
                    ENDPOINT_EVIDENCE_PREFIX,
                    now,
                )
            })
            .unwrap_or(i64::MIN)
    });
    let selected_endpoint_ids = endpoint_tasks
        .iter()
        .take(ENDPOINT_PROBE_BATCH)
        .map(|task| task.endpoint.as_str())
        .collect::<BTreeSet<_>>();
    let endpoint_probe_tasks = endpoint_tasks
        .iter()
        .filter(|task| selected_endpoint_ids.contains(task.endpoint.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let endpoint_probe_results = network_probes
        .probe_endpoints(&endpoint_probe_tasks, observed_at)
        .into_iter()
        .map(|status| (status.endpoint.clone(), status))
        .collect::<BTreeMap<_, _>>();
    for task in endpoint_tasks {
        let status = endpoint_probe_results
            .get(&task.endpoint)
            .cloned()
            .or_else(|| {
                previous_endpoints
                    .get(task.endpoint.as_str())
                    .and_then(|status| {
                        trusted_observation_ms(
                            &status.observed_at,
                            &status.message,
                            ENDPOINT_EVIDENCE_PREFIX,
                            now,
                        )
                        .map(|_| (*status).clone())
                    })
            })
            .unwrap_or_else(|| TopologyEndpointStatus {
                endpoint: task.endpoint.clone(),
                health: TopologyHealth::Unknown,
                reachable: false,
                latency_ms: None,
                message: "network probe: pending bounded observation batch".to_string(),
                observed_at: String::new(),
            });
        endpoint_statuses.insert(task.endpoint, status);
    }
    let endpoints = spec
        .endpoints
        .iter()
        .map(|endpoint| {
            let status = endpoint_statuses
                .remove(&endpoint.endpoint)
                .expect("every endpoint receives an observed status");
            if status.health != TopologyHealth::Healthy || !status.reachable {
                drift.push(TopologyDrift {
                    resource_kind: TopologyResourceKind::Endpoint,
                    resource_id: endpoint.endpoint.clone(),
                    kind: if status.message.starts_with("no runtime projection") {
                        TopologyDriftKind::Missing
                    } else {
                        TopologyDriftKind::Unreachable
                    },
                    detail: bounded_detail(&status.message),
                });
            }
            status
        })
        .collect::<Vec<_>>();

    let endpoint_status_by_id = endpoints
        .iter()
        .map(|status| (status.endpoint.as_str(), status))
        .collect::<BTreeMap<_, _>>();
    let endpoint_spec_by_id = spec
        .endpoints
        .iter()
        .map(|endpoint| (endpoint.endpoint.as_str(), endpoint))
        .collect::<BTreeMap<_, _>>();
    let previous_links = previous_status
        .map(|status| {
            status
                .links
                .iter()
                .map(|link| {
                    (
                        (link.source_endpoint.as_str(), link.target_endpoint.as_str()),
                        link,
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let mut link_tasks = Vec::new();
    let mut link_statuses = BTreeMap::new();
    for link in &spec.links {
        let key = (link.source_endpoint.clone(), link.target_endpoint.clone());
        if !link.enabled {
            link_statuses.insert(
                key,
                TopologyLinkStatus {
                    source_endpoint: link.source_endpoint.clone(),
                    target_endpoint: link.target_endpoint.clone(),
                    health: TopologyHealth::Unknown,
                    latency_ms: None,
                    message: "link is disabled and was not probed".to_string(),
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        if !link.api_bindings.is_empty() {
            let observed = link
                .api_bindings
                .iter()
                .map(|declared| {
                    api_bindings.iter().find(|binding| {
                        binding.requirement_name == declared.requirement_name
                            && binding.api_id == declared.api_id
                            && binding.link_source_endpoint == link.source_endpoint
                            && binding.link_target_endpoint == link.target_endpoint
                    })
                })
                .collect::<Vec<_>>();
            let healthy = observed.iter().all(|binding| {
                binding.is_some_and(|binding| {
                    binding.state == ApiBindingState::Active
                        && binding.desired_state == "ACTIVE"
                        && binding.observed_state == "ACTIVE"
                        && binding.health == "HEALTHY"
                })
            });
            link_statuses.insert(
                key,
                TopologyLinkStatus {
                    source_endpoint: link.source_endpoint.clone(),
                    target_endpoint: link.target_endpoint.clone(),
                    health: if healthy {
                        TopologyHealth::Healthy
                    } else {
                        TopologyHealth::Unhealthy
                    },
                    latency_ms: None,
                    message: if healthy {
                        "all ApiBindings are ACTIVE and healthy".to_string()
                    } else {
                        "one or more ApiBindings are missing, inactive, or unhealthy".to_string()
                    },
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        let source = endpoint_spec_by_id
            .get(link.source_endpoint.as_str())
            .expect("validated link source exists");
        if !link_probe_source_endpoints.contains(&link.source_endpoint) {
            link_statuses.insert(
                key,
                TopologyLinkStatus {
                    source_endpoint: link.source_endpoint.clone(),
                    target_endpoint: link.target_endpoint.clone(),
                    health: TopologyHealth::Unknown,
                    latency_ms: None,
                    message: format!(
                        "source endpoint {} has no exact release-bound orchestrator.link-probe.v1 capability",
                        link.source_endpoint
                    ),
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        let source_status = endpoint_status_by_id.get(link.source_endpoint.as_str());
        let target_status = endpoint_status_by_id.get(link.target_endpoint.as_str());
        if ![source_status, target_status]
            .into_iter()
            .flatten()
            .all(|status| status.health == TopologyHealth::Healthy && status.reachable)
            || source_status.is_none()
            || target_status.is_none()
        {
            link_statuses.insert(
                key,
                TopologyLinkStatus {
                    source_endpoint: link.source_endpoint.clone(),
                    target_endpoint: link.target_endpoint.clone(),
                    health: TopologyHealth::Unknown,
                    latency_ms: None,
                    message: "source or target endpoint lacks fresh healthy network evidence"
                        .to_string(),
                    observed_at: observed_at.to_string(),
                },
            );
            continue;
        }
        let target = endpoint_spec_by_id
            .get(link.target_endpoint.as_str())
            .expect("validated link target exists");
        link_tasks.push(LinkProbeTask {
            source_endpoint: link.source_endpoint.clone(),
            source_service_id: source.service_id.clone(),
            source_protocol: source.protocol.clone(),
            target_endpoint: link.target_endpoint.clone(),
            target_service_id: target.service_id.clone(),
        });
    }
    link_tasks.sort_by_key(|task| {
        previous_links
            .get(&(task.source_endpoint.as_str(), task.target_endpoint.as_str()))
            .and_then(|status| {
                trusted_observation_ms(
                    &status.observed_at,
                    &status.message,
                    LINK_EVIDENCE_PREFIX,
                    now,
                )
            })
            .unwrap_or(i64::MIN)
    });
    let selected_link_ids = link_tasks
        .iter()
        .take(LINK_PROBE_BATCH)
        .map(|task| (task.source_endpoint.as_str(), task.target_endpoint.as_str()))
        .collect::<BTreeSet<_>>();
    let link_probe_tasks = link_tasks
        .iter()
        .filter(|task| {
            selected_link_ids
                .contains(&(task.source_endpoint.as_str(), task.target_endpoint.as_str()))
        })
        .cloned()
        .collect::<Vec<_>>();
    let link_probe_results = network_probes
        .probe_links(&link_probe_tasks, observed_at)
        .into_iter()
        .map(|status| {
            (
                (
                    status.source_endpoint.clone(),
                    status.target_endpoint.clone(),
                ),
                status,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for task in link_tasks {
        let key = (task.source_endpoint.clone(), task.target_endpoint.clone());
        let status = link_probe_results.get(&key).cloned().or_else(|| {
            previous_links
                .get(&(task.source_endpoint.as_str(), task.target_endpoint.as_str()))
                .and_then(|status| {
                    trusted_observation_ms(
                        &status.observed_at,
                        &status.message,
                        LINK_EVIDENCE_PREFIX,
                        now,
                    )
                    .map(|_| (*status).clone())
                })
        });
        link_statuses.insert(
            key,
            status.unwrap_or_else(|| TopologyLinkStatus {
                source_endpoint: task.source_endpoint,
                target_endpoint: task.target_endpoint,
                health: TopologyHealth::Unknown,
                latency_ms: None,
                message: "source probe: pending bounded observation batch".to_string(),
                observed_at: String::new(),
            }),
        );
    }
    let links = spec
        .links
        .iter()
        .map(|link| {
            let status = link_statuses
                .remove(&(link.source_endpoint.clone(), link.target_endpoint.clone()))
                .expect("every link receives an observed status");
            if link.enabled && status.health != TopologyHealth::Healthy {
                drift.push(TopologyDrift {
                    resource_kind: TopologyResourceKind::Link,
                    resource_id: format!("{}->{}", link.source_endpoint, link.target_endpoint),
                    kind: TopologyDriftKind::Unreachable,
                    detail: bounded_detail(&status.message),
                });
            }
            status
        })
        .collect::<Vec<_>>();
    (endpoints, links)
}

pub(super) fn trusted_observation_ms(
    marker: &str,
    message: &str,
    evidence_prefix: &str,
    now: i64,
) -> Option<i64> {
    if !message.starts_with(evidence_prefix) {
        return None;
    }
    let observed = marker.strip_prefix("unix-ms:")?.parse::<i64>().ok()?;
    (observed <= now && now.saturating_sub(observed) <= NETWORK_OBSERVATION_MAX_AGE_MS)
        .then_some(observed)
}

pub(super) fn network_probe_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(NETWORK_PROBE_TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .build()
        .into()
}

pub(super) enum NetworkProbeWork {
    Endpoint {
        index: usize,
        task: EndpointProbeTask,
        observed_at: String,
        results: mpsc::Sender<NetworkProbeResult>,
    },
    Link {
        index: usize,
        task: LinkProbeTask,
        observed_at: String,
        results: mpsc::Sender<NetworkProbeResult>,
    },
    Shutdown,
}

pub(super) enum NetworkProbeResult {
    Endpoint(usize, TopologyEndpointStatus),
    Link(usize, TopologyLinkStatus),
}

pub(super) struct NetworkProbePool {
    pub(super) work: mpsc::SyncSender<NetworkProbeWork>,
    pub(super) workers: Vec<JoinHandle<()>>,
}

impl NetworkProbePool {
    pub(super) fn new() -> Self {
        let (work, receiver) = mpsc::sync_channel(LINK_PROBE_BATCH + ENDPOINT_PROBE_BATCH);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(NETWORK_PROBE_CONCURRENCY);
        for ordinal in 0..NETWORK_PROBE_CONCURRENCY {
            let receiver = Arc::clone(&receiver);
            workers.push(
                thread::Builder::new()
                    .name(format!("orchestrator-topology-probe-{ordinal:02}"))
                    .spawn(move || {
                        let agent = network_probe_agent();
                        loop {
                            let work = receiver
                                .lock()
                                .expect("network probe queue lock poisoned")
                                .recv();
                            match work {
                                Ok(NetworkProbeWork::Endpoint {
                                    index,
                                    task,
                                    observed_at,
                                    results,
                                }) => {
                                    let _ = results.send(NetworkProbeResult::Endpoint(
                                        index,
                                        probe_endpoint(&agent, &task, &observed_at),
                                    ));
                                }
                                Ok(NetworkProbeWork::Link {
                                    index,
                                    task,
                                    observed_at,
                                    results,
                                }) => {
                                    let _ = results.send(NetworkProbeResult::Link(
                                        index,
                                        probe_link(&agent, &task, &observed_at),
                                    ));
                                }
                                Ok(NetworkProbeWork::Shutdown) | Err(_) => break,
                            }
                        }
                    })
                    .expect("spawn fixed topology network probe worker"),
            );
        }
        Self { work, workers }
    }

    pub(super) fn probe_endpoints(
        &self,
        tasks: &[EndpointProbeTask],
        observed_at: &str,
    ) -> Vec<TopologyEndpointStatus> {
        let (results, receiver) = mpsc::channel();
        for (index, task) in tasks.iter().cloned().enumerate() {
            self.work
                .send(NetworkProbeWork::Endpoint {
                    index,
                    task,
                    observed_at: observed_at.to_string(),
                    results: results.clone(),
                })
                .expect("fixed topology network probe pool stopped unexpectedly");
        }
        drop(results);
        let mut observed = receiver
            .into_iter()
            .map(|result| match result {
                NetworkProbeResult::Endpoint(index, status) => (index, status),
                NetworkProbeResult::Link(_, _) => {
                    unreachable!("endpoint batch received a link probe result")
                }
            })
            .collect::<Vec<_>>();
        observed.sort_by_key(|(index, _)| *index);
        observed.into_iter().map(|(_, status)| status).collect()
    }

    pub(super) fn probe_links(
        &self,
        tasks: &[LinkProbeTask],
        observed_at: &str,
    ) -> Vec<TopologyLinkStatus> {
        let (results, receiver) = mpsc::channel();
        for (index, task) in tasks.iter().cloned().enumerate() {
            self.work
                .send(NetworkProbeWork::Link {
                    index,
                    task,
                    observed_at: observed_at.to_string(),
                    results: results.clone(),
                })
                .expect("fixed topology network probe pool stopped unexpectedly");
        }
        drop(results);
        let mut observed = receiver
            .into_iter()
            .map(|result| match result {
                NetworkProbeResult::Link(index, status) => (index, status),
                NetworkProbeResult::Endpoint(_, _) => {
                    unreachable!("link batch received an endpoint probe result")
                }
            })
            .collect::<Vec<_>>();
        observed.sort_by_key(|(index, _)| *index);
        observed.into_iter().map(|(_, status)| status).collect()
    }
}

impl Drop for NetworkProbePool {
    fn drop(&mut self) {
        for _ in 0..self.workers.len() {
            let _ = self.work.send(NetworkProbeWork::Shutdown);
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

pub(super) fn probe_endpoint(
    agent: &ureq::Agent,
    task: &EndpointProbeTask,
    observed_at: &str,
) -> TopologyEndpointStatus {
    let started = std::time::Instant::now();
    match endpoint_health_url(&task.endpoint, &task.protocol, &task.health_path)
        .and_then(|url| bounded_http_get(agent, &url).map(|_| url))
    {
        Ok(url) => TopologyEndpointStatus {
            endpoint: task.endpoint.clone(),
            health: TopologyHealth::Healthy,
            reachable: true,
            latency_ms: Some(elapsed_ms(started)),
            message: bounded_detail(&format!(
                "{ENDPOINT_EVIDENCE_PREFIX} {} {} returned HTTP 2xx for service {}",
                task.protocol, url, task.service_id
            )),
            observed_at: observed_at.to_string(),
        },
        Err(error) => TopologyEndpointStatus {
            endpoint: task.endpoint.clone(),
            health: TopologyHealth::Unhealthy,
            reachable: false,
            latency_ms: Some(elapsed_ms(started)),
            message: bounded_detail(&format!("{ENDPOINT_EVIDENCE_PREFIX} {error}")),
            observed_at: observed_at.to_string(),
        },
    }
}

pub(super) fn probe_link(
    agent: &ureq::Agent,
    task: &LinkProbeTask,
    observed_at: &str,
) -> TopologyLinkStatus {
    let started = std::time::Instant::now();
    let result = link_probe_url(
        &task.source_endpoint,
        &task.source_protocol,
        &task.target_endpoint,
    )
    .and_then(|url| bounded_http_get(agent, &url))
    .and_then(|body| validate_link_probe_body(task, &body));
    match result {
        Ok(()) => TopologyLinkStatus {
            source_endpoint: task.source_endpoint.clone(),
            target_endpoint: task.target_endpoint.clone(),
            health: TopologyHealth::Healthy,
            latency_ms: Some(elapsed_ms(started)),
            message: format!(
                "{LINK_EVIDENCE_PREFIX} source {} reached exact target {}",
                task.source_service_id, task.target_endpoint
            ),
            observed_at: observed_at.to_string(),
        },
        Err(error) => TopologyLinkStatus {
            source_endpoint: task.source_endpoint.clone(),
            target_endpoint: task.target_endpoint.clone(),
            health: TopologyHealth::Unhealthy,
            latency_ms: Some(elapsed_ms(started)),
            message: bounded_detail(&format!("{LINK_EVIDENCE_PREFIX} {error}")),
            observed_at: observed_at.to_string(),
        },
    }
}

pub(super) fn endpoint_health_url(
    endpoint: &str,
    protocol: &str,
    path: &str,
) -> Result<String, String> {
    endpoint_url(endpoint, protocol, path, None)
}

pub(super) fn link_probe_url(source: &str, protocol: &str, target: &str) -> Result<String, String> {
    validate_endpoint_id(target).map_err(|error| format!("invalid target endpoint: {error}"))?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("target", target)
        .finish();
    endpoint_url(source, protocol, "/probe", Some(&query))
}

pub(super) fn endpoint_url(
    endpoint: &str,
    protocol: &str,
    path: &str,
    query: Option<&str>,
) -> Result<String, String> {
    validate_endpoint_id(endpoint).map_err(|error| error.to_string())?;
    if !matches!(protocol, "http" | "https") {
        return Err(format!(
            "protocol {protocol} does not expose the v1 HTTP network probe contract"
        ));
    }
    if !path.starts_with('/') || path.contains('#') {
        return Err("health/probe path must be an absolute path without a fragment".to_string());
    }
    let identity = parse_endpoint_id(endpoint).map_err(|error| error.to_string())?;
    let host = if identity.host.contains(':') {
        format!("[{}]", identity.host)
    } else {
        identity.host.to_string()
    };
    let mut url = url::Url::parse(&format!("{protocol}://{host}:{}", identity.port))
        .map_err(|error| format!("construct endpoint URL: {error}"))?;
    url.set_path(path);
    url.set_query(query);
    Ok(url.to_string())
}

pub(super) fn bounded_http_get(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let response = agent
        .get(url)
        .header("accept", "application/json")
        .call()
        .map_err(|error| format!("GET {url} failed: {error}"))?;
    let status = response.status().as_u16();
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(NETWORK_RESPONSE_LIMIT as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| format!("GET {url} response read failed: {error}"))?;
    if body.len() > NETWORK_RESPONSE_LIMIT {
        return Err(format!(
            "GET {url} response exceeded {NETWORK_RESPONSE_LIMIT} bytes"
        ));
    }
    if !(200..=299).contains(&status) {
        return Err(format!("GET {url} returned HTTP {status}"));
    }
    Ok(body)
}

pub(super) fn validate_link_probe_body(task: &LinkProbeTask, body: &[u8]) -> Result<(), String> {
    let value: Value =
        serde_json::from_slice(body).map_err(|error| format!("decode /probe JSON: {error}"))?;
    let expected = [
        ("status", "healthy"),
        ("source_service_id", task.source_service_id.as_str()),
        ("target_endpoint", task.target_endpoint.as_str()),
        ("target_service_id", task.target_service_id.as_str()),
    ];
    if expected
        .iter()
        .any(|(key, expected)| value.get(key).and_then(Value::as_str) != Some(*expected))
    {
        return Err("/probe response does not prove the exact source-to-target path".to_string());
    }
    Ok(())
}

pub(super) fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
