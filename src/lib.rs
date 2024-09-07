use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use aws::{ShardDetector, ShardPoller};
use aws_config::BehaviorVersion;
use aws_sdk_kinesis::{types::Record, Client};
use serde::{Deserialize, Serialize};
use spin_core::InstancePre;
use spin_trigger::{cli::NoArgs, TriggerAppEngine, TriggerExecutor};

mod aws;

wasmtime::component::bindgen!({
    path: "kinesis.wit",
    async: true
});

use fermyon::spin_kinesis::kinesis_types as kinesis;
use tokio::sync::mpsc;

pub(crate) type RuntimeData = ();

pub struct KinesisTrigger {
    engine: TriggerAppEngine<Self>,
    queue_components: Vec<Component>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KinesisTriggerConfig {
    pub component: String,
    pub stream_arn: String,
    pub batch_size: Option<u16>,
    pub shard_idle_wait_seconds: Option<u64>,
    pub detector_poll_seconds: Option<u64>,
}

#[derive(Clone, Debug)]
struct Component {
    pub id: Arc<String>,
    pub stream_arn: Arc<String>,
    pub batch_size: u16,
    pub shard_idle_wait_seconds: tokio::time::Duration,
    pub detector_poll_seconds: tokio::time::Duration,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TriggerMetadata {
    r#type: String,
}

// This is a placeholder - we don't yet detect any situations that would require
// graceful or ungraceful exit.  It will likely require rework when we do.  It
// is here so that we have a skeleton for returning errors that doesn't expose
// us to thoughtlessly "?"-ing away an Err case and creating a situation where a
// transient failure could end the trigger.
#[allow(dead_code)]
#[derive(Debug)]
enum TerminationReason {
    ExitRequested,
    Other(String),
}

#[async_trait]
impl TriggerExecutor for KinesisTrigger {
    const TRIGGER_TYPE: &'static str = "kinesis";
    type RuntimeData = RuntimeData;
    type TriggerConfig = KinesisTriggerConfig;
    type RunConfig = NoArgs;
    type InstancePre = InstancePre<RuntimeData>;

    async fn new(engine: TriggerAppEngine<Self>) -> Result<Self> {
        let queue_components = engine
            .trigger_configs()
            .map(|(_, config)| Component {
                id: Arc::new(config.component.clone()),
                stream_arn: Arc::new(config.stream_arn.clone()),
                batch_size: config.batch_size.unwrap_or(10),
                shard_idle_wait_seconds: tokio::time::Duration::from_secs(
                    config.shard_idle_wait_seconds.unwrap_or(2),
                ),
                detector_poll_seconds: tokio::time::Duration::from_secs(
                    config.detector_poll_seconds.unwrap_or(30),
                ),
            })
            .collect();

        Ok(Self {
            engine,
            queue_components,
        })
    }

    async fn run(self, _config: Self::RunConfig) -> Result<()> {
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.unwrap();
            std::process::exit(0);
        });

        let config = aws_config::load_defaults(BehaviorVersion::latest()).await;

        let client = Client::new(&config);
        let engine = Arc::new(self.engine);

        let loops = self
            .queue_components
            .iter()
            .map(|component| Self::start_receive_loop(engine.clone(), &client, component));

        let (tr, _, rest) = futures::future::select_all(loops).await;
        drop(rest);

        match tr {
            Ok(TerminationReason::ExitRequested) => {
                tracing::trace!("Exiting");
                Ok(())
            }
            _ => {
                tracing::trace!("Fatal: {:?}", tr);
                Err(anyhow::anyhow!("{tr:?}"))
            }
        }
    }
}

impl KinesisTrigger {
    fn start_receive_loop(
        engine: Arc<TriggerAppEngine<Self>>,
        client: &Client,
        component: &Component,
    ) -> tokio::task::JoinHandle<TerminationReason> {
        let future = Self::receive(engine, client.clone(), component.clone());
        tokio::task::spawn(future)
    }

    // This doesn't return a Result because we don't want a thoughtless `?` to exit the loop
    // and terminate the entire trigger.  Termination should be a conscious decision when
    // we are sure there is no point continuing.
    async fn receive(
        engine: Arc<TriggerAppEngine<Self>>,
        client: Client,
        Component {
            stream_arn,
            batch_size,
            shard_idle_wait_seconds,
            detector_poll_seconds,
            id,
        }: Component,
    ) -> TerminationReason {
        let (tx_new_shard, mut rx_new_shard) = mpsc::channel(4);
        let (tx_finished_shard, rx_finished_shard) = mpsc::channel(4);

        // Spawn a task to send new shards to the receiver
        let shard_detector = ShardDetector::new(
            &stream_arn,
            detector_poll_seconds,
            &client,
            rx_finished_shard,
            tx_new_shard,
        );
        tokio::spawn(shard_detector.poll_new_shards());

        // Main event loop -- spawn a poller for each new shard received
        loop {
            if let Some(shard_id) = rx_new_shard.recv().await {
                let shard_poller = ShardPoller::new(
                    &engine,
                    &id,
                    &tx_finished_shard,
                    &client,
                    &stream_arn,
                    shard_id,
                    batch_size,
                    shard_idle_wait_seconds,
                );
                tokio::spawn(shard_poller.poll_records());
            } else {
                // The channel was closed, so the shard detector task has exited
                return TerminationReason::Other("Shard detector task exited".to_string());
            }
        }
    }
}

struct KinesisRecordProcessor {
    engine: Arc<TriggerAppEngine<KinesisTrigger>>,
    component_id: Arc<String>,
}

impl KinesisRecordProcessor {
    fn new(engine: &Arc<TriggerAppEngine<KinesisTrigger>>, component_id: &Arc<String>) -> Self {
        Self {
            engine: engine.clone(),
            component_id: component_id.clone(),
        }
    }

    async fn process_records(&self, records: Vec<Record>) {
        let records = records
            .into_iter()
            .map(|record| kinesis::KinesisRecord {
                sequence_number: record.sequence_number,
                data: kinesis::Blob {
                    inner: record.data.into_inner(),
                },
            })
            .collect::<Vec<_>>();

        let action = self.execute_wasm(&records).await;

        match action {
            Ok(_) => {
                tracing::trace!("Records processed successfully");
            }
            Err(e) => {
                tracing::error!("Records processing error: {}", e.to_string());
            }
        }
    }

    async fn execute_wasm(&self, records: &[kinesis::KinesisRecord]) -> Result<()> {
        let component_id = &self.component_id;
        let (instance, mut store) = self.engine.prepare_instance(component_id).await?;

        let instance = SpinKinesis::new(&mut store, &instance)?;

        match instance
            .call_handle_batch_records(&mut store, records)
            .await
        {
            Ok(Ok(action)) => {
                tracing::trace!("Component {component_id} completed okay");
                Ok(action)
            }
            Ok(Err(e)) => {
                tracing::warn!("Component {component_id} returned error {:?}", e);
                Err(anyhow::anyhow!(
                    "Component {component_id} returned error processing records"
                )) // TODO: more details when WIT provides them
            }
            Err(e) => {
                tracing::error!("Engine error running component {component_id}: {:?}", e);
                Err(anyhow::anyhow!(
                    "Error executing component {component_id} while processing records"
                ))
            }
        }
    }
}
