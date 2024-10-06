title = "How I Wrote A Kinesis Trigger Plugin For Spin"
date = "2024-10-08T16:00:00Z"
template = "blog"
tags = ["spin", "rust", "kinesis", "aws", "plugins"]
description = "Learn why and how I built an AWS Kinesis trigger for Spin."

[extra]
author = "Darwin Boersma"
author_image = "/static/image/profile/darwin-boersma.jpg"
twitter_card_type = "summary_large_image"
type = "post"
category = "community"

---

Hi there! My name is Darwin and I have worked on microservice architectures of various sizes throughout my career (Docker-in-Docker, AWS Lambda). I recently became a member of the Spin community and contributed an [AWS Kinesis trigger](https://developer.fermyon.com/hub/preview/plugin_spin_kinesis_trigger) – I’d love to share a bit about why. I have worked on data pipelines where consistency is important. When your entire product is providing quality data, you care a lot whether your data gets from point A to point B. But how do you ensure that happens? Platforms like Kafka are purpose-built for this use-case, offering powerful guarantees on durability and receipt of data. Kinesis is the AWS cloud solution to this challenge.

<!-- break -->

## My Introduction To Spin

A common approach to creating real-time data pipelines in AWS is to use AWS Lambda functions to parse data from Kinesis – the function will be triggered nearly instantaneously after data is added to the Kinesis stream, and this solution offers complete flexibility on how you handle that data. This approach works pretty well: function-as-a-service\* frameworks have numerous benefits.

> :eight_spoked_asterisk: Function-as-a-service generally offers: automatically scale to zero at low volume, automatically scale up rapidly under load, integrate with distributed services (like Kinesis), compose code across languages with low effort. You can learn more about FaaS in [Fermyon’s Serverless Guide](https://www.fermyon.com/serverless-guide/index)

_But_, serverless options like Lambda still have challenges:

1. Scaling can cause unpredictable cold starts as a VM is spun up for your new function execution
2. Cold starts can, later in a project lifetime, become an insurmountable barrier due to new requirements (e.g. new functionality with a low SLA requirement). This can cause teams to undertake additional effort to migrate code to hosted servers
3. Testing and observability require significant additional effort – many common debugging mechanisms are unusable. Tracking call chains across services requires extensive configuration in the code for each service. Running a Lambda locally is highly restrictive (or impossible, depending on language), and sandboxing resources (like VPC) have no mechanism to simulate locally.

How do we keep the advantages of serverless architecture but fix these pain points? Well, Webassembly (Wasm) is proving to be a compelling foundational technology for the next iteration of serverless architectures. Fermyon has been very interesting to follow as a pioneer in this space – Fermyon Spin is a modular serverless framework powered by the efficiency and security of Wasm.

If you aren’t familiar with server-side Wasm, no worries - you don’t have to be an expert! Here are a few reasons why Spin stands out to me as a developer tool for writing event-driven functions powered by Wasm.

- **Upleveling performance with polyglot capabilities -** Spin lets your Python/JavaScript/X services seamlessly invoke (or be invoked by) efficient Rust functions.
- **Portable dependency injection** – the same function might invoke a SQLite database locally but invoke a distributed SQL service when deployed to the cloud without requiring any modifications to the application’s source code.
- **Observability with batteries included -** If you have a current set of observability tools you know and love for your existing functions and containerized workloads, the good news is Spin has been making rapid advances to fit into your existing developer toolkit. Recently, as of Spin 2.4, experimental [Otel integration was released](https://www.fermyon.com/blog/otel-plugin) as well as a powerful [testing framework](https://www.fermyon.com/blog/announcing-spin-test).
- **Extensible -** Spin has an extensible trigger / handler model that makes it easy to connect your Spin applications to external dependencies. HTTP and Redis triggers in Spin are excellent – and the folks at Spin have kindly published an [SQS trigger](https://github.com/fermyon/spin-trigger-sqs/) providing a model for AWS integration!

Having worked with AWS Kinesis [Lambda event sources](https://docs.aws.amazon.com/whitepapers/latest/security-overview-aws-lambda/lambda-event-sources.html) and [guarantees this model offers](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/streamsmain.html#streamsmain.choose) that enable high volume data processing, I wanted to take a crack at providing a similar integration of Kinesis for Spin. I am excited to see Spin and Wasm supercharge stream processing and serverless data pipelines!

## How The Kinesis Trigger Works

Kinesis streams (like Kafka) are generally used for high-volume, rapid data transfer. Common use cases are processing large quantities of data + metrics, syncing updates on a database to another database in real time (also known as change data capture), and durably storing events for replay. The Spin Kinesis trigger aims to replicate the ease of consuming Kinesis data inside AWS cloud via Fermyon Spin applications – if you are familiar with creating a Lambda handler for Kinesis, you will be right at home creating a Spin handler for Kinesis. The [WIT interface](https://github.com/ogghead/spin-trigger-kinesis/blob/main/kinesis.wit) for consuming data is [modeled directly after the Kinesis API](https://docs.aws.amazon.com/kinesis/latest/APIReference/API_Record.html) – [like the AWS Lambda event source](https://docs.aws.amazon.com/sdk-for-rust/latest/dg/rust_kinesis_code_examples.html#serverless_examples), your handler simply takes in a list of records ([link to full example](https://github.com/ogghead/spin-trigger-kinesis/blob/main/guest/src/lib.rs)).

```rust
async fn handle_batch_records(records: Vec<KinesisRecord>)-> Result(), Error> {
      for record in records {
            // do something with each record
      }
}
```

Like the Lambda event source, the Kinesis trigger guarantees that data in each shard will be processed in sequential batches, and that each shard will be processed independently. High-volume, ordered data processing in Spin with Kinesis!

## Kinesis Trigger In Action

Now that we’ve built up some context on how to consume Kinesis data, let’s walk through how you can set this up on your own Spin application.

> Prerequisites: You will need a provisioned AWS Kinesis instance ([tutorial](https://docs.aws.amazon.com/streams/latest/dev/tutorial-stock-data-kplkcl2.html))

Using this plugin requires only a few simple steps:

1. Install the plugin

```sh
spin plugins update
spin plugins install trigger-kinesis
```

2. Install the template

```sh
spin templates install --git https://github.com/ogghead/spin-trigger-kinesis
```

3. Create your spin application with the Spin CLI using the template, filling out the require parameters

> Note that only the ARN parmeter is required. The remaining are set with the following defaults

```sh
spin new -t kinesis-rust hello-kinesis

Description: Simple spin app that handles kinesis events
ARN: rn:aws:kinesis:us-east-1:123456789012:stream/TestStream
Batch Size: 10
Batch Size: 1000
detector poll millis (ms): 30000
Shard Iterator Type: AT_SEQUENCE_NUMBER
```

4. Implement your component's business logic to handle the Kinesis event data (for exampe, in Rust)

```rust
use spin_kinesis_sdk::{kinesis_component, Error, KinesisRecord};


#[kinesis_component]
async fn handle_batch_records(records: Vec<KinesisRecord>) -> Result<(), Error> {
    for record in records {
        println!("I GOT A RECORD!  ID: {:?}", record.sequence_number);
        let data = String::from_utf8(record.data.inner).unwrap();
        println!("  ... DATA: {:?}", data);
    }
    Ok(())
}
```

5. Build and run your Spin application to begin handling Kinesis events

```sh
spin build --up
```

Congratulations! You now have a running Spin application that’s ready to begin handling Kinesis events.

So how does it perform? In the version 0.3 beta, I added OTel support, generated some mock data for my Kinesis instance, and gave the Kinesis trigger a spin:

![Traces from the Kinesis trigger](/static/image/blog/spin-kinesis-process-records-otel.png)

Whoa! Despite this metric capturing _a wrapper function around executing the component that does additional work_, the component is spinning up and beginning processing in under a millisecond.

> :eight_spoked_asterisk: - For a full accounting of performance, I want to test a component involving network connections, and compare a replica Lambda function. Testing under ramping load, at variable load, and with rapid spikes would also help to give a more full picture. That is a story to tell another day – stay tuned!

## Publishing and Distributing

Before we wrap up, I’d be remiss if I didn’t share a bit about my experience authoring a Spin plugin. Creating and publishing a plugin was refreshingly streamlined! By referring to the [SQS trigger](https://github.com/fermyon/spin-trigger-sqs/) published by Spin, I answered a few questions for myself, and hopefully someone else too.

Q: How does a trigger read in spin.toml configuration?

```rust
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FooTriggerConfig {
    pub component: String,
    pub foo_custom_field_1: String,
    pub foo_custom_field_2: Option<u16>,
}

#[derive(Clone, Debug)]
struct Component {
    pub id: String,
    pub foo_custom_field_1: String,
    pub foo_custom_field_2: Option<u16>,
}

#[async_trait]
impl TriggerExecutor for FooTrigger {
    const TRIGGER_TYPE: &static str = "footrigger";
    type TriggerConfig = FooTriggerConfig;

    ...

    async fn new(engine: TriggerAppEngine<Self>) -> Result<Self> {
        let queue_components = engine
            .trigger_configs()
            .map(|config| Component {
                id: config.component,
                foo_custom_field_1: config.foo_custom_field_1,
                foo_custom_field_2: config.foo_custom_field_2.unwrap_or(10),
            })
            .collect();

        Ok(Self {
            engine,
            queue_components,
        })
    }
}
```

A: Structs and a trait\! The setup is short, idiomatic Rust.

Q: How does a trigger execute components?

```rust
async fn execute_wasm(&self, data: &FooData) -> Result<()> {
    let component_id = &self.component_id;
    let (instance, mut store) = self.engine.prepare_instance(component_id).await?; // Spin prepares component to execute

    let instance = SpinFoo::new(&mut store, &instance)?; // Create an instance of the WASI interface for your component, linking it with the provided component to execute

    match instance
        .call_handle_foo(&mut store, records)
        .await {
            // Trigger handles response from Spin component
    }
}
```

A: Here is an example setup!

After cleaning up, testing, and merging a branch with my Kinesis changes into my repository’s main branch, I followed [the steps](https://developer.fermyon.com/spin/v2/plugin-authoring#packaging-a-plugin) to create a pull request to the [spin-plugins repository](https://github.com/fermyon/spin-plugins) – and that’s it. My plugin is now available for download!

## What’s Next

I am still working on enhancements to the trigger, and I plan to continue to release incremental updates to it. For version 0.3, I have added support for all shard iterator types as well as OTel integration through the excellent [Spin OTel plugin](https://www.fermyon.com/blog/otel-plugin).

AWS has a lot of distributed services and we have only scratched the surface on the integrations possible with Spin – what would a DynamoDB Streams trigger look like? How about EventBridge? S3? There are many possibilities. Spin also offers the prospect of pluggable interfaces enabling developers to utilize cloud services or local services seamlessly, and lays the foundations for a new type of developer experience. If you’re curious on how to create a plugin, the [Spin documentation](https://developer.fermyon.com/spin/v2/plugin-authoring) is excellent and the folks at Spin are very welcoming over at [Discord](https://discord.com/invite/AAFNfS7NGf).
