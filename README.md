# Experimental Kinesis trigger for Fermyon Spin

This is a [Fermyon Spin](https://www.fermyon.com/spin) trigger for [AWS Kinesis](https://docs.aws.amazon.com/streams/latest/dev/introduction.html) -- a solution for streaming bulk loads of real-time data.

## Install the latest release

The latest stable release of the kinesis trigger plugin can be installed like so:

```sh
spin plugins update
spin plugins install trigger-kinesis
```

## Install the canary version

The canary release of the kinesis trigger plugin represents the most recent commits on `main` and may not be stable, with some features still in progress.

```sh
spin plugins install --url https://github.com/ogghead/spin-trigger-kinesis/releases/download/canary/trigger-kinesis.json
```

## Build from source

You will need Rust and the `pluginify` plugin (`spin plugins install --url https://github.com/itowlson/spin-pluginify/releases/download/canary/pluginify.json`).

```sh
cargo build --release
spin pluginify --install
```

## Installing Template

You can install the template using the following command:

```bash
spin templates install --git https://github.com/ogghead/spin-trigger-kinesis
```

Once the template is installed, you can create a new application using:

```bash
spin new -t kinesis-rust hello_kinesis --accept-defaults
```

## Test

The trigger uses the AWS configuration environment variables - these must be set before running.
Be sure to set `AWS_DEFAULT_REGION` in your environment to the region of your stream.

You will also need to change the `stream_arn` in `spin.toml` to a stream you have access to.

```sh
cd guest
spin build --up
```

## Limitations

This trigger is currently built using Spin 2.6. You will need that version of Spin or above.

Custom triggers, such as this one, can be run in the Spin command line, but cannot be deployed to Fermyon Cloud.  For other hosts, check the documentation.

## Configuration

The kinesis trigger uses the AWS credentials from the standard AWS configuration environment variables.  These variables must be set before you run `spin up`.  The credentials must grant access to all streams that the application wants to monitor.  The credentials must allow for reading records.

The trigger assumes that the monitored streams exist: it does not create them.

### `spin.toml`

The trigger type is `kinesis`, and there are no application-level configuration options.

The following options are available to set in the `[[trigger.kinesis]]` section:

| Name                        | Type             | Required? | Description |
|-----------------------------|------------------|-----------|-------------|
| `stream_arn`                | string           | required  | The stream to which this trigger listens and responds. |
| `batch_size`                | number           | optional  | The maximum number of records to fetch per Kinesis shard on each poll. The default is 10. This directly affects the amount of records that your component is invoked with. |
| `shard_idle_wait_millis`    | number           | optional  | How long (in milliseconds) to wait between checks when the stream is idle (i.e. when no messages were received on the last check). The default is 1000. If the stream is _not_ idle, there is no wait between checks. The idle wait is also applied if an error occurs. Note that this number should _not_ exceed 300,000 milliseconds (5 minutes), as shard iterators time out after this period |
| `detector_poll_millis`      | number           | optional  | How long (in milliseconds) to wait between checks for new shards. The default is 30,000 (30 seconds). |
| `shard_iterator_type`       | enum             | optional  | See <https://docs.aws.amazon.com/cli/latest/reference/kinesis/get-shard-iterator.html#options> for possible options. Defaults to LATEST. |
| `component`                 | string or table  | required  | The component to run when a stream record is received. This is the standard Spin trigger component field. |

For example:

```toml
spin_manifest_version = 2

[application]
name = "test"
version = "0.1.0"

# One [[trigger.kinesis]] section for each stream to monitor
[[trigger.kinesis]]
component = "test"
stream_arn = "arn:aws:kinesis:us-east-1:1234567890:stream/TestStream"
batch_size = 1000
shard_idle_wait_millis = 250
detector_poll_millis = 30000
shard_iterator_type = "TRIM_HORIZON"

[component.test]
source = "..."
```

### `spin up` command line options

There are no custom command line options for this trigger.

## Writing kinesis components

There is no SDK for kinesis guest components.  Use the `kinesis.wit` file to generate a trigger binding for your language.  Your Wasm component must _export_ the `handle-batch-records` function.  See `guest/src/lib.rs`  for how to do this in Rust.

**Note:** In the current WIT, a record directly matches [the AWS Kinesis record shape](https://docs.aws.amazon.com/kinesis/latest/APIReference/API_Record.html). This contains the content of the record encoded as binary. Feedback is welcome on this design decision.

Your handler can return an error, but should otherwise not return anything.

**Note:** Shards are processed in order, but there is no guarantee of processing ordering between shards.
