use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_kinesis::{types::Record, Client};
use futures::{StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use spin_core::InstancePre;
use spin_trigger::{cli::NoArgs, TriggerAppEngine, TriggerExecutor};

mod aws;

wasmtime::component::bindgen!({
    path: "kinesis.wit",
    async: true
});

use fermyon::spin_kinesis::kinesis_types as kinesis;

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
    pub shard_record_limit: Option<u16>,
    pub idle_wait_seconds: Option<u64>,
}

#[derive(Clone, Debug)]
struct Component {
    pub id: String,
    pub stream_arn: String,
    pub shard_record_limit: u16,
    pub idle_wait: tokio::time::Duration,
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
                id: config.component.clone(),
                stream_arn: config.stream_arn.clone(),
                shard_record_limit: config.shard_record_limit.unwrap_or(10),
                idle_wait: tokio::time::Duration::from_secs(config.idle_wait_seconds.unwrap_or(2)),
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
        component: Component,
    ) -> TerminationReason {
        // TODO: Poll on a cadence for new shards and add them to the list of shards to poll
        let shards = match client
            .list_shards()
            .stream_arn(&component.stream_arn)
            .send()
            .await
        {
            Ok(shards) => shards,
            Err(e) => {
                tracing::error!(
                    "Stream {}: error listing shards: {:?}",
                    component.stream_arn,
                    e
                );
                return TerminationReason::Other("Error listing shards".to_owned());
            }
        };

        // create a poller for each shard
        let shard_pollers: Result<Vec<_>> = tokio_stream::iter(shards.shards())
            .map(|shard| {
                aws::ShardPoller::new(
                    client.clone(),
                    &component.stream_arn,
                    &shard.shard_id,
                    component.shard_record_limit,
                )
            })
            .then(|poller| poller.make_ready())
            .try_collect()
            .await;

        let Ok(mut shard_pollers) = shard_pollers else {
            return TerminationReason::Other("Error creating shard pollers".to_owned());
        };

        loop {
            let records: Vec<Record> = tokio_stream::iter(shard_pollers.iter_mut())
                .then(|poller| poller.get_records())
                .flat_map(tokio_stream::iter)
                .collect()
                .await;

            if records.is_empty() {
                tokio::time::sleep(component.idle_wait).await;
            } else {
                for record in records {
                    // TODO: Should we process records in order?
                    let processor = KinesisRecordProcessor::new(&engine, &component);
                    tokio::spawn(async move { processor.process_record(record).await });
                }
            }
        }
    }
}

struct KinesisRecordProcessor {
    engine: Arc<TriggerAppEngine<KinesisTrigger>>,
    component: Component,
}

impl KinesisRecordProcessor {
    fn new(engine: &Arc<TriggerAppEngine<KinesisTrigger>>, component: &Component) -> Self {
        Self {
            engine: engine.clone(),
            component: component.clone(),
        }
    }

    async fn process_record(
        &self,
        Record {
            sequence_number,
            data,
            ..
        }: Record,
    ) {
        tracing::trace!("Record {sequence_number}: spawned processing task");

        let blob = kinesis::Blob {
            inner: data.into_inner(),
        };
        let record = kinesis::KinesisRecord {
            sequence_number: sequence_number.clone(),
            data: blob,
        };

        let action = self.execute_wasm(record).await;

        match action {
            Ok(_) => {
                tracing::trace!("Record {sequence_number} processed successfully");
            }
            Err(e) => {
                tracing::error!(
                    "Record {sequence_number} processing error: {}",
                    e.to_string()
                );
                // TODO: change message visibility to 0 I guess?
            }
        }
    }

    async fn execute_wasm(&self, record: kinesis::KinesisRecord) -> Result<()> {
        let record_id = record.sequence_number.clone();
        let component_id = &self.component.id;
        tracing::trace!("Message {record_id}: executing component {component_id}");
        let (instance, mut store) = self.engine.prepare_instance(component_id).await?;

        let instance = SpinKinesis::new(&mut store, &instance)?;

        match instance
            .call_handle_stream_message(&mut store, &record)
            .await
        {
            Ok(Ok(action)) => {
                tracing::trace!("Record {record_id}: component {component_id} completed okay");
                Ok(action)
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    "Record {record_id}: component {component_id} returned error {:?}",
                    e
                );
                Err(anyhow::anyhow!(
                    "Component {component_id} returned error processing record {record_id}"
                )) // TODO: more details when WIT provides them
            }
            Err(e) => {
                tracing::error!(
                    "Record {record_id}: engine error running component {component_id}: {:?}",
                    e
                );
                Err(anyhow::anyhow!(
                    "Error executing component {component_id} while processing record {record_id}"
                ))
            }
        }
    }
}
