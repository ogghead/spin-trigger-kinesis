pub use spin_kinesis_macro::kinesis_component;

#[doc(hidden)]
pub use spin_executor as executor;

#[doc(hidden)]
pub mod wit {
    #![allow(missing_docs)]

    wit_bindgen::generate!({
        world: "spin-kinesis-sdk",
        path: "..",
    });
}

#[doc(inline)]
pub use wit::fermyon::spin_kinesis::kinesis_types::Blob;
#[doc(inline)]
pub use wit::fermyon::spin_kinesis::kinesis_types::EncryptionType;
#[doc(inline)]
pub use wit::fermyon::spin_kinesis::kinesis_types::Error;
#[doc(inline)]
pub use wit::fermyon::spin_kinesis::kinesis_types::KinesisRecord;

#[doc(hidden)]
pub use wit_bindgen;
