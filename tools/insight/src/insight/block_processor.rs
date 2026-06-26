use std::collections::VecDeque;

#[derive(Default)]
pub(crate) struct BlockProcessorViewModel {
    pub recently_processed: VecDeque<String>,
}
