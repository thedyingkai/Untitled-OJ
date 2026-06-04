mod checker;
mod config;
mod db;
mod judge;
mod problem_package;
mod result;
mod sandbox;

use anyhow::{Context, Result};
use redis::streams::StreamReadReply;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use crate::config::LanguagesConfig;
use crate::judge::handle_submission;

const JUDGE_STREAM: &str = "ojos:judge:submissions";
const JUDGE_GROUP: &str = "judge-workers";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().json().init();

    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://ojos-redis:6379/0".to_string());

    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://postgres:password@postgres:5432/ojos?sslmode=disable".to_string()
    });

    let languages_path =
        std::env::var("LANGUAGES_CONFIG").unwrap_or_else(|_| "config/languages.yaml".to_string());

    let consumer_name = std::env::var("JUDGE_WORKER_ID")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "judge-worker-local".to_string());

    let languages = Arc::new(
        LanguagesConfig::load(&languages_path)
            .await
            .context("load languages config failed")?,
    );

    info!(
        %redis_url,
        %database_url,
        %languages_path,
        %consumer_name,
        "judge-worker starting"
    );

    let db = PgPool::connect(&database_url)
        .await
        .context("connect postgres failed")?;

    let redis_client =
        redis::Client::open(redis_url.clone()).context("create redis client failed")?;

    {
        let mut conn = redis_client
            .get_multiplexed_async_connection()
            .await
            .context("connect redis failed")?;

        let pong: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .context("ping redis failed")?;

        info!(pong = %pong, "connected redis successfully");
    }

    ensure_consumer_group(&redis_client).await?;

    if let Err(err) = scan_pending_submissions(&db, languages.clone(), 100).await {
        error!(error = %err, "initial pending scan failed");
    }

    {
        let scan_db = db.clone();
        let scan_languages = languages.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;

                if let Err(err) =
                    scan_pending_submissions(&scan_db, scan_languages.clone(), 100).await
                {
                    error!(error = %err, "periodic pending scan failed");
                }
            }
        });
    }

    info!(
        stream = JUDGE_STREAM,
        group = JUDGE_GROUP,
        consumer = %consumer_name,
        "judge-worker consuming redis stream"
    );

    loop {
        let mut conn = redis_client
            .get_multiplexed_async_connection()
            .await
            .context("connect redis for stream read failed")?;

        let read_result: redis::RedisResult<StreamReadReply> = redis::cmd("XREADGROUP")
            .arg("GROUP")
            .arg(JUDGE_GROUP)
            .arg(&consumer_name)
            .arg("COUNT")
            .arg(1)
            .arg("BLOCK")
            .arg(5000)
            .arg("STREAMS")
            .arg(JUDGE_STREAM)
            .arg(">")
            .query_async(&mut conn)
            .await;

        let reply = match read_result {
            Ok(reply) => reply,
            Err(err) if err.is_timeout() || err.to_string().contains("timed out") => {
                continue;
            }
            Err(err) => {
                error!(error = %err, "xreadgroup failed");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        if reply.keys.is_empty() {
            continue;
        }

        for stream_key in reply.keys {
            for stream_id in stream_key.ids {
                let message_id = stream_id.id.clone();
                let submission_id = parse_submission_id_from_stream(&stream_id.map);

                info!(
                    stream = %stream_key.key,
                    message_id = %message_id,
                    submission_id = ?submission_id,
                    "received judge stream message"
                );

                if let Some(submission_id) = submission_id {
                    if let Err(err) = handle_submission(&db, languages.clone(), submission_id).await
                    {
                        error!(
                            submission_id,
                            error = %err,
                            "judge submission failed"
                        );

                        let _ = crate::db::mark_submission_failed(
                            &db,
                            submission_id,
                            "SYSTEM_ERROR",
                            &err.to_string(),
                        )
                        .await;
                    }
                } else {
                    warn!(message_id = %message_id, "judge stream message missing valid submission_id");
                }

                if let Err(err) = ack_stream_message(&mut conn, &message_id).await {
                    error!(
                        message_id = %message_id,
                        error = %err,
                        "ack judge stream message failed"
                    );
                }
            }
        }
    }
}

async fn ensure_consumer_group(redis_client: &redis::Client) -> Result<()> {
    let mut conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .context("connect redis for group create failed")?;

    let result: redis::RedisResult<()> = redis::cmd("XGROUP")
        .arg("CREATE")
        .arg(JUDGE_STREAM)
        .arg(JUDGE_GROUP)
        .arg("$")
        .arg("MKSTREAM")
        .query_async(&mut conn)
        .await;

    match result {
        Ok(_) => {
            info!(
                stream = JUDGE_STREAM,
                group = JUDGE_GROUP,
                "redis stream consumer group created"
            );
        }
        Err(err) if err.to_string().contains("BUSYGROUP") => {
            info!(
                stream = JUDGE_STREAM,
                group = JUDGE_GROUP,
                "redis stream consumer group already exists"
            );
        }
        Err(err) => {
            return Err(err).context("create redis stream consumer group failed");
        }
    }

    Ok(())
}

async fn ack_stream_message(
    conn: &mut redis::aio::MultiplexedConnection,
    message_id: &str,
) -> Result<()> {
    let acked: i64 = redis::cmd("XACK")
        .arg(JUDGE_STREAM)
        .arg(JUDGE_GROUP)
        .arg(message_id)
        .query_async(conn)
        .await
        .context("xack failed")?;

    info!(message_id = %message_id, acked, "judge stream message acked");

    Ok(())
}

fn parse_submission_id_from_stream(map: &HashMap<String, redis::Value>) -> Option<i64> {
    let value = map.get("submission_id")?;
    let text: String = redis::from_redis_value(value.clone()).ok()?;
    text.parse::<i64>().ok()
}

async fn scan_pending_submissions(
    db: &PgPool,
    languages: Arc<LanguagesConfig>,
    limit: i64,
) -> Result<()> {
    let ids = crate::db::list_pending_submission_ids(db, limit).await?;

    if ids.is_empty() {
        return Ok(());
    }

    info!(count = ids.len(), "pending submissions found");

    for submission_id in ids {
        if let Err(err) = handle_submission(db, languages.clone(), submission_id).await {
            error!(
                submission_id,
                error = %err,
                "failed to handle pending submission"
            );

            let _ = crate::db::mark_submission_failed(
                db,
                submission_id,
                "SYSTEM_ERROR",
                &err.to_string(),
            )
            .await;
        }
    }

    Ok(())
}
