#[cfg(feature = "test_features")]
pub(crate) mod adv {
    use crate::client::AdvProduceBlocksMode;

    #[derive(Default)]
    struct ClientControls {
        adv_produce_blocks: Option<AdvProduceBlocksMode>,
        produce_invalid_chunks: bool,
        produce_invalid_tx_in_chunks: bool,
        disable_header_sync: std::sync::atomic::AtomicBool,
        disable_doomslug: std::sync::atomic::AtomicBool,
        is_archival: bool,
    }
}

#[cfg(not(feature = "test_features"))]
pub(crate) mod adv {
    #[derive(Default, Clone)]
    pub struct Controls;

    impl crate::adversarial::Controls {
        pub const fn new(_is_archival: bool) -> Self {
            Self
        }

        pub const fn disable_header_sync(&self) -> bool {
            false
        }

        pub const fn disable_doomslug(&self) -> bool {
            false
        }
    }
}
