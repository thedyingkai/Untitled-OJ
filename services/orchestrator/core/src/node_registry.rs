//! Deterministic registry projections and node graph validation.
//! The compatibility-memory and durable-upsert entry points retain their existing error ordering.
use crate::{
    DeployedServiceApi, EffectiveApiRoute, NodeRecord, OrchestratorError, Result, ServiceApiSurface,
};
use std::collections::{BTreeMap, BTreeSet};

pub fn validate_node_tree_upsert<'a>(
    existing_nodes: impl Iterator<Item = &'a NodeRecord>,
    node: &NodeRecord,
) -> Result<()> {
    let mut nodes = existing_nodes
        .map(|item| (item.node_id.clone(), item.clone()))
        .collect::<BTreeMap<_, _>>();
    if nodes
        .values()
        .any(|item| item.node_id != node.node_id && item.host_ip == node.host_ip)
    {
        return Err(OrchestratorError::InvalidManifest(format!(
            "node host_ip {} is already registered",
            node.host_ip
        )));
    }
    match node.role.as_str() {
        "root" | "standalone" => {
            if !node.parent_node_id.trim().is_empty() {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "{} node must not have parent_node_id",
                    node.role
                )));
            }
        }
        "node" => {
            if node.parent_node_id.trim().is_empty() {
                return Err(OrchestratorError::InvalidManifest(
                    "node parent_node_id is required".to_string(),
                ));
            }
            if !nodes.contains_key(&node.parent_node_id) {
                return Err(OrchestratorError::Dependency(format!(
                    "parent node {} not found",
                    node.parent_node_id
                )));
            }
        }
        _ => {}
    }
    nodes.insert(node.node_id.clone(), node.clone());
    ensure_node_tree_acyclic(&nodes)
}

pub fn ensure_node_tree_acyclic(nodes: &BTreeMap<String, NodeRecord>) -> Result<()> {
    for node_id in nodes.keys() {
        let mut seen = BTreeSet::new();
        let mut current = node_id.as_str();
        loop {
            if !seen.insert(current.to_string()) {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "node tree contains cycle at {current}"
                )));
            }
            let Some(node) = nodes.get(current) else {
                return Err(OrchestratorError::Dependency(format!(
                    "node {current} is missing during tree validation"
                )));
            };
            let parent = node.parent_node_id.trim();
            if parent.is_empty() {
                break;
            }
            if !nodes.contains_key(parent) {
                return Err(OrchestratorError::Dependency(format!(
                    "parent node {parent} not found"
                )));
            }
            current = parent;
        }
    }
    Ok(())
}

pub fn ancestors_of_from_nodes(nodes: Vec<NodeRecord>, node_id: &str) -> Result<Vec<NodeRecord>> {
    let map = nodes
        .into_iter()
        .map(|node| (node.node_id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    ensure_node_tree_acyclic(&map)?;
    let node = map
        .get(node_id)
        .ok_or_else(|| OrchestratorError::Dependency(format!("node {node_id} not found")))?;
    if node.role == "standalone" {
        return Ok(Vec::new());
    }
    let mut ancestors = Vec::new();
    let mut parent_id = node.parent_node_id.trim().to_string();
    while !parent_id.is_empty() {
        let parent = map.get(&parent_id).ok_or_else(|| {
            OrchestratorError::Dependency(format!("parent node {parent_id} not found"))
        })?;
        ancestors.push(parent.clone());
        parent_id = parent.parent_node_id.trim().to_string();
    }
    Ok(ancestors)
}

pub fn descendants_of_from_nodes(nodes: Vec<NodeRecord>, node_id: &str) -> Result<Vec<NodeRecord>> {
    let map = nodes
        .into_iter()
        .map(|node| (node.node_id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    ensure_node_tree_acyclic(&map)?;
    if !map.contains_key(node_id) {
        return Err(OrchestratorError::Dependency(format!(
            "node {node_id} not found"
        )));
    }
    let mut descendants = Vec::new();
    let mut frontier = vec![node_id.to_string()];
    while let Some(parent_id) = frontier.pop() {
        let mut children = map
            .values()
            .filter(|node| node.parent_node_id == parent_id)
            .cloned()
            .collect::<Vec<_>>();
        children.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        for child in children {
            frontier.push(child.node_id.clone());
            descendants.push(child);
        }
    }
    Ok(descendants)
}

pub fn effective_api_routes_from_registry(
    node_id: &str,
    nodes: Vec<NodeRecord>,
    surfaces: Vec<ServiceApiSurface>,
    deployed_apis: Vec<DeployedServiceApi>,
) -> Result<Vec<EffectiveApiRoute>> {
    let node_by_id = nodes
        .iter()
        .cloned()
        .map(|node| (node.node_id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    ensure_node_tree_acyclic(&node_by_id)?;
    let target = node_by_id
        .get(node_id)
        .ok_or_else(|| OrchestratorError::Dependency(format!("node {node_id} not found")))?;
    let node_by_host = nodes
        .iter()
        .cloned()
        .map(|node| (node.host_ip.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let surface_by_key = surfaces
        .into_iter()
        .map(|api| {
            (
                (
                    api.service_name.clone(),
                    api.version.clone(),
                    api.api_id.clone(),
                ),
                api,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let ancestor_distances = ancestors_of_from_nodes(nodes, node_id)?
        .into_iter()
        .enumerate()
        .map(|(index, ancestor)| (ancestor.node_id, (index + 1) as u32))
        .collect::<BTreeMap<_, _>>();
    let mut routes = Vec::new();
    for deployed in deployed_apis
        .into_iter()
        .filter(|deployed| deployed.status == "running")
    {
        let Some(provider_node) = node_by_host.get(&deployed.host_ip) else {
            continue;
        };
        let Some(surface) = surface_by_key.get(&(
            deployed.service_name.clone(),
            deployed.version.clone(),
            deployed.api_id.clone(),
        )) else {
            continue;
        };
        let (visible, distance, visibility_source) = if provider_node.node_id == target.node_id {
            if matches!(surface.visibility.as_str(), "same-node" | "global") {
                (true, 0, "same-node")
            } else {
                (false, 0, "")
            }
        } else if let Some(distance) = ancestor_distances.get(&provider_node.node_id) {
            if surface.visibility == "descendants" {
                (true, *distance, "ancestor-descendants")
            } else {
                (false, *distance, "")
            }
        } else {
            (false, 0, "")
        };
        if !visible {
            continue;
        }
        routes.push(EffectiveApiRoute {
            node_id: target.node_id.clone(),
            api_id: surface.api_id.clone(),
            provider_node_id: provider_node.node_id.clone(),
            provider_host_ip: provider_node.host_ip.clone(),
            provider_service_name: surface.service_name.clone(),
            provider_endpoint: deployed.endpoint.clone(),
            protocol: surface.protocol.clone(),
            path_prefix: surface.path_prefix.clone(),
            methods: surface.methods.clone(),
            permission: surface.permission.clone(),
            auth_mode: surface.auth_mode.clone(),
            visibility_source: visibility_source.to_string(),
            distance,
            status: deployed.status.clone(),
        });
    }
    routes.sort_by(|left, right| {
        left.distance
            .cmp(&right.distance)
            .then_with(|| left.api_id.cmp(&right.api_id))
            .then_with(|| left.provider_endpoint.cmp(&right.provider_endpoint))
    });
    Ok(routes)
}

pub fn validate_node_tree(mut nodes: Vec<NodeRecord>, candidate: &NodeRecord) -> Result<()> {
    if nodes
        .iter()
        .any(|node| node.node_id != candidate.node_id && node.host_ip == candidate.host_ip)
    {
        return Err(OrchestratorError::InvalidManifest(format!(
            "node host_ip {} is already registered",
            candidate.host_ip
        )));
    }
    match candidate.role.as_str() {
        "root" | "standalone" if !candidate.parent_node_id.trim().is_empty() => {
            return Err(OrchestratorError::InvalidManifest(format!(
                "{} node must not have parent_node_id",
                candidate.role
            )));
        }
        "node" if candidate.parent_node_id.trim().is_empty() => {
            return Err(OrchestratorError::InvalidManifest(
                "node parent_node_id is required".to_string(),
            ));
        }
        _ => {}
    }
    nodes.retain(|node| node.node_id != candidate.node_id);
    nodes.push(candidate.clone());
    let by_id = nodes
        .into_iter()
        .map(|node| (node.node_id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    for node in by_id.values() {
        let mut current = node;
        let mut seen = BTreeSet::new();
        while !current.parent_node_id.trim().is_empty() {
            if !seen.insert(current.node_id.clone()) {
                return Err(OrchestratorError::InvalidManifest(format!(
                    "node tree contains cycle at {}",
                    current.node_id
                )));
            }
            current = by_id.get(&current.parent_node_id).ok_or_else(|| {
                OrchestratorError::Dependency(format!(
                    "parent node {} not found",
                    current.parent_node_id
                ))
            })?;
        }
    }
    Ok(())
}
