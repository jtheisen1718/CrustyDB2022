use crate::page::Page;
use common::ids::PageId;
use common::{CrustyError, PAGE_SIZE};
use std::fs::{File, OpenOptions};
use std::io::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, RwLock};

use std::io::{Seek, SeekFrom};

/// The struct for a heap file.  
///
/// HINT: You likely will want to design for interior mutability for concurrent accesses.
/// eg Arc<RwLock<>> on some internal members
///
/// HINT: You will probably not be able to serialize HeapFile, as it needs to maintain a link to a
/// File object, which cannot be serialized/deserialized/skipped by serde. You don't need to worry
/// about persisting read_count/write_count during serialization.
///
/// Your code should persist what information is needed to recreate the heapfile.
///
pub(crate) struct HeapFile {
    pub f: Arc<RwLock<File>>,
    pub file_path: String,
    pub page_ids: RwLock<Vec<PageId>>,
    // The following are for profiling/ correctness checks
    pub read_count: AtomicU16,
    pub write_count: AtomicU16,
}

#[derive(Serialize, Deserialize)]
pub struct SerializableHeapFile {
    pub file_path: String,
    pub page_ids: Vec<PageId>,
}

/// HeapFile required functions
impl HeapFile {
    /// Create a new heapfile for the given path and container Id. Return Result<Self> if able to create.
    /// Errors could arise from permissions, space, etc when trying to create the file used by HeapFile.
    pub(crate) fn new(file_path: PathBuf) -> Result<Self, CrustyError> {
        // Open heapfile
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&file_path)
        {
            Ok(f) => f,
            Err(error) => {
                return Err(CrustyError::CrustyError(format!(
                    "Cannot open or create heap file: {} {} {:?}",
                    file_path.to_string_lossy(),
                    error,
                    error
                )))
            }
        };

        Ok(HeapFile {
            f: Arc::new(RwLock::new(file)),
            file_path: file_path.to_str().unwrap().to_string(),
            page_ids: RwLock::new(Vec::new()),
            read_count: AtomicU16::new(0),
            write_count: AtomicU16::new(0),
        })
    }

    /// Return the number of pages for this HeapFile.
    /// Return type is PageId (alias for another type) as we cannot have more
    /// pages than PageId can hold.
    pub fn num_pages(&self) -> PageId {
        return self.page_ids.read().unwrap().len() as PageId;
    }

    /// Read the page from the file.
    /// Errors could arise from the filesystem or invalid pageId
    pub(crate) fn read_page_from_file(&self, pid: PageId) -> Result<Page, CrustyError> {
        //If profiling count reads
        #[cfg(feature = "profile")]
        {
            self.read_count.fetch_add(1, Ordering::Relaxed);
        }

        // Check for invalid page_id
        if !(self.page_ids.read().unwrap().contains(&pid)) {
            return Err(CrustyError::CrustyError(format!(
                "Invalid pageID: {}",
                &pid
            )));
        };

        // Create byte array to hold the page
        let mut buffer: [u8; PAGE_SIZE] = [0; PAGE_SIZE];
        // Find the index of page_id in the page_ids vector, which corresponds
        // to the index of the byte block of size 4096 that the page is
        // serialized to
        let index;
        {
            index = self
                .page_ids
                .read()
                .unwrap()
                .iter()
                .position(|&r| r == pid)
                .unwrap();
        }
        {
            let mut file = self.f.write().unwrap();

            // Seek to page
            if let Err(e) = file.seek(SeekFrom::Start((index * PAGE_SIZE).try_into().unwrap())) {
                return Err(CrustyError::CrustyError(format!(
                    "Cannot seek through heapfile: {:?}",
                    e
                )));
            };
            // Read to buffer
            if let Err(e) = file.read(&mut buffer) {
                return Err(CrustyError::CrustyError(format!(
                    "Cannot read heapfile: {:?}",
                    e
                )));
            };
        }
        // Turn buffer into page object and return it.
        let page = Page::from_bytes(&buffer);
        Ok(page)
    }

    /// Take a page and write it to the underlying file.
    /// This could be an existing page or a new page
    pub(crate) fn write_page_to_file(&self, page: Page) -> Result<(), CrustyError> {
        //If profiling count writes
        #[cfg(feature = "profile")]
        {
            self.write_count.fetch_add(1, Ordering::Relaxed);
        }

        let page_bytes: &[u8] = &(page.get_bytes());
        let index;
        {
            let mut p_ids = self.page_ids.write().unwrap();
            if !p_ids.contains(&page.p_id) {
                p_ids.push(page.p_id)
            }
            index = p_ids.iter().position(|&r| r == page.p_id).unwrap();
        }

        let mut file = self.f.write().unwrap();
        if let Err(e) = file.seek(SeekFrom::Start((index * PAGE_SIZE).try_into().unwrap())) {
            return Err(CrustyError::CrustyError(format!(
                "Cannot seek through heapfile: {:?}",
                e
            )));
        };
        if let Err(e) = file.write(page_bytes) {
            return Err(CrustyError::CrustyError(format!(
                "Cannot write to heapfile: {:?}",
                e
            )));
        };
        Ok(())
    }
}

#[cfg(test)]
#[allow(unused_must_use)]
mod test {
    use super::*;
    use common::testutil::*;
    use temp_testdir::TempDir;

    #[test]
    fn hs_hf_insert() {
        init();

        //Create a temp file
        let f = gen_random_dir();
        let tdir = TempDir::new(f, true);
        let mut f = tdir.to_path_buf();
        f.push(gen_rand_string(4));
        f.set_extension("hf");

        let mut hf = HeapFile::new(f.to_path_buf()).unwrap();

        // Make a page and write
        let mut p0 = Page::new(0);
        let bytes = get_random_byte_vec(100);
        p0.add_value(&bytes);
        let bytes = get_random_byte_vec(100);
        p0.add_value(&bytes);
        let bytes = get_random_byte_vec(100);
        p0.add_value(&bytes);
        let p0_bytes = p0.get_bytes();

        hf.write_page_to_file(p0);
        //check the page
        assert_eq!(1, hf.num_pages());
        let checkp0 = hf.read_page_from_file(0).unwrap();
        assert_eq!(p0_bytes, checkp0.get_bytes());

        //Add another page
        let mut p1 = Page::new(1);
        let bytes = get_random_byte_vec(100);
        p1.add_value(&bytes);
        let bytes = get_random_byte_vec(100);
        p1.add_value(&bytes);
        let bytes = get_random_byte_vec(100);
        p1.add_value(&bytes);
        let p1_bytes = p1.get_bytes();

        hf.write_page_to_file(p1);

        assert_eq!(2, hf.num_pages());
        //Recheck page0
        let checkp0 = hf.read_page_from_file(0).unwrap();
        assert_eq!(p0_bytes, checkp0.get_bytes());

        //check page 1
        let checkp1 = hf.read_page_from_file(1).unwrap();
        assert_eq!(p1_bytes, checkp1.get_bytes());

        #[cfg(feature = "profile")]
        {
            assert_eq!(*hf.read_count.get_mut(), 3);
            assert_eq!(*hf.write_count.get_mut(), 2);
        }
    }
}
