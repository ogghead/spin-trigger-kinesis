wit_bindgen::generate!({
    world: "spin-kinesis",
    path: "..",
    exports: {
        world: Kinesis
    }
});

struct Kinesis;

impl Guest for Kinesis {
    fn handle_stream_message(record: KinesisRecord) -> Result<(), Error> {
        println!("I GOT A RECORD!  ID: {:?}", record.sequence_number);
        println!("  ... DATA: {:?}", record.data);

        Ok(())
    }
}
