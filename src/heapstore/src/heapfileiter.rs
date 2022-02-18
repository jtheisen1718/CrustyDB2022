use crate::heapfile::HeapFile;
use crate::page::Page;
use crate::page::PageIter;
use common::ids::{ContainerId, PageId, TransactionId};
use std::sync::Arc;

#[allow(dead_code)]
/// The struct for a HeapFileIterator.
/// We use a slightly different approach for HeapFileIterator than
/// standard way of Rust's IntoIter for simplicity (avoiding lifetime issues).
/// This should store the state/metadata required to iterate through the file.
///
/// HINT: This will need an Arc<HeapFile>
pub struct HeapFileIterator {
    //TODO milestone hs
    hf: Arc<HeapFile>,
    page_index: PageId,
    page_iter: PageIter,
}

/// Required HeapFileIterator functions
impl HeapFileIterator {
    /// Create a new HeapFileIterator that stores the container_id, tid, and heapFile pointer.
    /// This should initialize the state required to iterate through the heap file.
    pub(crate) fn new(container_id: ContainerId, tid: TransactionId, hf: Arc<HeapFile>) -> Self {
        if hf.num_pages() == 0{
            return HeapFileIterator {
                hf: hf,
                page_index: 0,
                page_iter: Page::new(0).into_iter(),
            }
        };
        let page;
        {
            let p_ids = hf.page_ids.read().unwrap();
            page = hf.read_page_from_file(p_ids[0]).unwrap();
        }
        return HeapFileIterator{
            hf: hf,
            page_index: 0,
            page_iter: page.into_iter(),
        };
    }
}

/// Trait implementation for heap file iterator.
/// Note this will need to iterate through the pages and their respective iterators.
impl Iterator for HeapFileIterator {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.page_index >= self.hf.num_pages(){
            return None;
        };
        let this = self.page_iter.next();
        if this == None{
            self.page_index += 1;
            let page;
            {
                let p_ids = self.hf.page_ids.read().unwrap();
                page = self.hf.read_page_from_file(p_ids[self.page_index as usize]).unwrap();
            }
            self.page_iter = page.into_iter();
            return self.next();
        };
        return this;
    }
}
