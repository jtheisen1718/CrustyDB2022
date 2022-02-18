use crate::heapfile::HeapFile;
use crate::heapfile::SerializableHeapFile;
use crate::heapfileiter::HeapFileIterator;
use crate::page::Page;
use common::prelude::*;
use common::storage_trait::StorageTrait;
use common::testutil::gen_random_dir;
use common::PAGE_SIZE;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::BufReader;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};

/// The StorageManager struct
#[derive(Serialize, Deserialize)]
pub struct StorageManager {
    #[serde(skip)]
    containers: Arc<RwLock<HashMap<ContainerId, Arc<HeapFile>>>>,
    /// Path to database metadata files.
    pub storage_path: String,
    is_temp: bool,
}

/// The required functions in HeapStore's StorageManager that are specific for HeapFiles
impl StorageManager {
    /// Get a page if exists for a given container.
    pub(crate) fn get_page(
        &self,
        container_id: ContainerId,
        page_id: PageId,
        _tid: TransactionId,
        _perm: Permissions,
        _pin: bool,
    ) -> Option<Page> {
        match self.containers.write().unwrap().get(&container_id) {
            Some(h) => match h.read_page_from_file(page_id) {
                Ok(p) => return Some(p),
                _ => return None,
            },
            None => return None,
        }
    }

    /// Write a page
    pub(crate) fn write_page(
        &self,
        container_id: ContainerId,
        page: Page,
        _tid: TransactionId,
    ) -> Result<(), CrustyError> {
        match self.containers.write().unwrap().get(&container_id) {
            Some(h) => return h.write_page_to_file(page),
            None => {
                return Err(common::CrustyError::CrustyError(
                    "Invalid container_id".to_string(),
                ))
            }
        }
    }

    /// Get the number of pages for a container
    fn get_num_pages(&self, container_id: ContainerId) -> PageId {
        match self.containers.read().unwrap().get(&container_id) {
            Some(h) => return h.num_pages(),
            None => panic!("get_num_pages: Container did not exist!"),
        }
    }

    /// Test utility function for counting reads and writes served by the heap file.
    /// Can return 0,0 for invalid container_ids
    #[allow(dead_code)]
    pub(crate) fn get_hf_read_write_count(&self, container_id: ContainerId) -> (u16, u16) {
        match self.containers.read().unwrap().get(&container_id) {
            Some(h) => (
                h.read_count.load(Ordering::Relaxed),
                h.write_count.load(Ordering::Relaxed),
            ),
            None => (0u16, 0u16),
        }
    }
}

/// Implementation of storage trait
impl StorageTrait for StorageManager {
    type ValIterator = HeapFileIterator;

    /// Create a new storage manager that will use storage_path as the location to persist data
    /// (if the storage manager persists records on disk)
    fn new(storage_path: String) -> Self {
        std::fs::create_dir_all(&storage_path);

        let mut metadata_path = PathBuf::from(&storage_path);
        metadata_path.push("metadata");

        if !metadata_path.exists() {
            return StorageManager {
                storage_path: storage_path,
                is_temp: false,
                containers: Arc::new(RwLock::new(HashMap::new())),
            };
        } else {
            // Deserialize
            let metadata = fs::OpenOptions::new()
                .read(true)
                .write(false)
                .create(false)
                .open(&metadata_path)
                .unwrap();
            let reader = BufReader::new(metadata);
            let deser_heapfiles: HashMap<ContainerId, SerializableHeapFile> =
                serde_json::from_reader(reader).unwrap();
            let mut heapfiles: HashMap<ContainerId, Arc<HeapFile>> = HashMap::new();
            for cid in deser_heapfiles.keys() {
                let dehf = deser_heapfiles.get(cid).unwrap();
                let mut hf = HeapFile::new(PathBuf::from(&dehf.file_path)).unwrap();
                hf.page_ids = RwLock::new(dehf.page_ids.clone());
                heapfiles.insert(*cid, Arc::new(hf));
            }
            return StorageManager {
                containers: Arc::new(RwLock::new(heapfiles)),
                storage_path: storage_path,
                is_temp: false,
            };
        }
    }

    /// Create a new storage manager for testing. If this creates a temporary directory it should be cleaned up
    /// when it leaves scope.
    fn new_test_sm() -> Self {
        let storage_path = gen_random_dir().to_string_lossy().to_string();
        debug!("Making new temp storage_manager {}", storage_path);
        std::fs::create_dir_all(&storage_path);
        StorageManager {
            storage_path: storage_path,
            is_temp: true,
            containers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn get_simple_config() -> common::ContainerConfig {
        common::ContainerConfig::simple_container()
    }

    /// Insert some bytes into a container for a particular value (e.g. record).
    /// Any validation will be assumed to happen before.
    /// Returns the value id associated with the stored value.
    /// Function will need to find the first page that can hold the value.
    /// A new page may need to be created if no space on existing pages can be found.
    fn insert_value(
        &self,
        container_id: ContainerId,
        value: Vec<u8>,
        tid: TransactionId,
    ) -> ValueId {
        if value.len() > PAGE_SIZE {
            panic!("Cannot handle inserting a value larger than the page size");
        }
        let containers = self.containers.write().unwrap();
        let heapfile = containers.get(&container_id).unwrap();
        let p_ids = heapfile.page_ids.write().unwrap();

        for p_id in p_ids.iter() {
            let mut page = heapfile.read_page_from_file(*p_id).unwrap();
            if page.get_largest_free_contiguous_space() - 6 >= value.len() {
                let mut v_id = ValueId::new(container_id);
                v_id.page_id = Some(page.get_page_id());
                v_id.slot_id = page.add_value(&&value);
                self.write_page(container_id, page, tid);
                return v_id;
            }
        }
        let p_id: PageId;
        if p_ids.len() == 0 {
            p_id = 0;
        } else {
            p_id = p_ids[p_ids.len() - 1] + 1;
        }
        let mut v_id = ValueId::new(container_id);
        let mut page = Page::new(p_id);
        v_id.page_id = Some(page.get_page_id());
        v_id.slot_id = page.add_value(&value);
        self.write_page(container_id, page, tid);
        return v_id;
    }

    /// Insert some bytes into a container for vector of values (e.g. record).
    /// Any validation will be assumed to happen before.
    /// Returns a vector of value ids associated with the stored values.
    fn insert_values(
        &self,
        container_id: ContainerId,
        values: Vec<Vec<u8>>,
        tid: TransactionId,
    ) -> Vec<ValueId> {
        let mut value_ids = Vec::new();
        for val in values.iter() {
            value_ids.push(self.insert_value(container_id, val.to_vec(), tid));
        }
        return value_ids;
    }

    /// Delete the data for a value. If the valueID is not found it returns Ok() still.
    fn delete_value(&self, id: ValueId, tid: TransactionId) -> Result<(), CrustyError> {
        match self.containers.write().unwrap().get(&id.container_id) {
            Some(hf) => {
                let mut page = hf.read_page_from_file(id.page_id.unwrap()).unwrap();
                page.delete_value(id.slot_id.unwrap());
                hf.write_page_to_file(page);
                return Ok(());
            }
            None => panic!("Invalid Container Id"),
        }
    }

    /// Updates a value. Returns valueID on update (which may have changed). Error on failure
    /// Any process that needs to determine if a value changed will need to compare the return valueId against
    /// the sent value.
    fn update_value(
        &self,
        value: Vec<u8>,
        id: ValueId,
        _tid: TransactionId,
    ) -> Result<ValueId, CrustyError> {
        self.delete_value(id, _tid);
        return Ok(self.insert_value(id.container_id, value, _tid));
    }

    /// Create a new container to be stored.
    /// fn create_container(&self, name: String) -> ContainerId;
    /// Creates a new container object.
    /// For this milestone you will not need to utilize
    /// the container_config, name, container_type, or dependencies
    ///
    ///
    /// # Arguments
    ///
    /// * `container_id` - Id of container to add delta to.
    fn create_container(
        &self,
        container_id: ContainerId,
        _container_config: common::ContainerConfig,
        _name: Option<String>,
        _container_type: common::ids::StateType,
        _dependencies: Option<Vec<ContainerId>>,
    ) -> Result<(), CrustyError> {
        let mut path = PathBuf::from(self.storage_path.clone());
        path.push(format!("heapfile{}", container_id));
        let heapfile = Arc::new(HeapFile::new(path).unwrap());
        self.containers
            .write()
            .unwrap()
            .insert(container_id, heapfile);
        return Ok(());
    }

    /// A wrapper function to call create container
    fn create_table(&self, container_id: ContainerId) -> Result<(), CrustyError> {
        self.create_container(
            container_id,
            StorageManager::get_simple_config(),
            None,
            common::ids::StateType::BaseTable,
            None,
        )
    }

    /// Remove the container and all stored values in the container.
    /// If the container is persisted remove the underlying files
    fn remove_container(&self, container_id: ContainerId) -> Result<(), CrustyError> {
        let mut containers = self.containers.write().unwrap();
        match fs::remove_file(format!("heapfile{}", container_id)) {
            Ok(_) => {
                containers.remove(&container_id);
                return Ok(());
            }
            _ => {
                return Err(common::CrustyError::CrustyError(format!(
                    "Failed to remove file: heapfile{}",
                    container_id
                )))
            }
        }
    }

    /// Get an iterator that returns all valid records
    fn get_iterator(
        &self,
        container_id: ContainerId,
        tid: TransactionId,
        _perm: Permissions,
    ) -> Self::ValIterator {
        let containers = self.containers.read().unwrap();
        let hf = containers.get(&container_id).unwrap();
        return HeapFileIterator::new(container_id, tid, Arc::clone(hf));
    }

    /// Get the data for a particular ValueId. Error if does not exists
    fn get_value(
        &self,
        id: ValueId,
        tid: TransactionId,
        perm: Permissions,
    ) -> Result<Vec<u8>, CrustyError> {
        match self.containers.read().unwrap().get(&id.container_id) {
            Some(hf) => {
                let page = hf.read_page_from_file(id.page_id.unwrap()).unwrap();
                return Ok(page.get_value(id.slot_id.unwrap()).unwrap());
            }
            _ => {
                return Err(common::CrustyError::CrustyError(
                    "Invalid container id".to_string(),
                ))
            }
        }
    }

    /// Notify the storage manager that the transaction is finished so that any held resources can be released.
    fn transaction_finished(&self, tid: TransactionId) {
        panic!("TODO milestone tm");
    }

    /// Testing utility to reset all state associated the storage manager.
    fn reset(&self) -> Result<(), CrustyError> {
        let mut containers = self.containers.write().unwrap();
        containers.clear();
        fs::remove_dir_all(self.storage_path.clone()).unwrap();
        return Ok(());
    }

    /// If there is a buffer pool or cache it should be cleared/reset.
    fn clear_cache(&self) {
        panic!("TODO milestone hs");
    }

    /// Shutdown the storage manager. Can call drop. Should be safe to call multiple times.
    /// If temp, this should remove all stored files.
    /// If not a temp SM, this should serialize the mapping between containerID and Heapfile.
    /// HINT: Heapfile won't be serializable/deserializable. You'll want to serialize information
    /// that can be used to create a HeapFile object pointing to the same data. You don't need to
    /// worry about recreating read_count or write_count.
    fn shutdown(&self) {
        let mut metadata_path = PathBuf::from(&self.storage_path);
        metadata_path.push("metadata");

        let metadata = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&metadata_path)
            .unwrap();
        {
            let mut ser_heapfiles: HashMap<ContainerId, SerializableHeapFile> = HashMap::new();
            let containers = self.containers.read().unwrap();
            for cid in containers.keys() {
                let hf = containers.get(cid).unwrap();
                let ser_hf = SerializableHeapFile {
                    file_path: hf.file_path.clone(),
                    page_ids: hf.page_ids.read().unwrap().to_vec(),
                };
                ser_heapfiles.insert(*cid, ser_hf);
            }
            serde_json::to_writer(metadata, &ser_heapfiles);
        }
        self.containers.write().unwrap().clear();
        drop(self);
    }

    fn import_csv(
        &self,
        table: &Table,
        path: String,
        _tid: TransactionId,
        container_id: ContainerId,
        _timestamp: LogicalTimeStamp,
    ) -> Result<(), CrustyError> {
        // Convert path into an absolute path.
        let path = fs::canonicalize(path)?;
        debug!("server::csv_utils trying to open file, path: {:?}", path);
        let file = fs::File::open(path)?;
        // Create csv reader.
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_reader(file);

        // Iterate through csv records.
        let mut inserted_records = 0;
        for result in rdr.records() {
            #[allow(clippy::single_match)]
            match result {
                Ok(rec) => {
                    // Build tuple and infer types from schema.
                    let mut tuple = Tuple::new(Vec::new());
                    for (field, attr) in rec.iter().zip(table.schema.attributes()) {
                        // TODO: Type mismatch between attributes and record data>
                        match &attr.dtype() {
                            DataType::Int => {
                                let value: i32 = field.parse::<i32>().unwrap();
                                tuple.field_vals.push(Field::IntField(value));
                            }
                            DataType::String => {
                                let value: String = field.to_string().clone();
                                tuple.field_vals.push(Field::StringField(value));
                            }
                        }
                    }
                    //TODO: How should individual row insertion errors be handled?
                    debug!(
                        "server::csv_utils about to insert tuple into container_id: {:?}",
                        &container_id
                    );
                    self.insert_value(container_id, tuple.get_bytes(), _tid);
                    inserted_records += 1;
                }
                _ => {
                    // FIXME: get error from csv reader
                    error!("Could not read row from CSV");
                    return Err(CrustyError::IOError(
                        "Could not read row from CSV".to_string(),
                    ));
                }
            }
        }
        info!("Num records imported: {:?}", inserted_records);
        Ok(())
    }
}

/// Trait Impl for Drop
impl Drop for StorageManager {
    /// Shutdown the storage manager. Can call be called by shutdown. Should be safe to call multiple times.
    /// If temp, this should remove all stored files.
    fn drop(&mut self) {
        if self.is_temp {
            debug!("Removing storage path on drop {}", self.storage_path);
            fs::remove_dir_all(self.storage_path.clone()).unwrap();
        }
    }
}

#[cfg(test)]
#[allow(unused_must_use)]
mod test {
    use super::*;
    use crate::storage_manager::StorageManager;
    use common::storage_trait::StorageTrait;
    use common::testutil::*;

    #[test]
    fn hs_sm_a_insert() {
        init();
        let sm = StorageManager::new_test_sm();
        let cid = 1;
        sm.create_table(cid);

        let bytes = get_random_byte_vec(40);
        let tid = TransactionId::new();

        let val1 = sm.insert_value(cid, bytes.clone(), tid);
        assert_eq!(1, sm.get_num_pages(cid));
        assert_eq!(0, val1.page_id.unwrap());
        assert_eq!(0, val1.slot_id.unwrap());

        let p1 = sm
            .get_page(cid, 0, tid, Permissions::ReadOnly, false)
            .unwrap();

        let val2 = sm.insert_value(cid, bytes, tid);
        assert_eq!(1, sm.get_num_pages(cid));
        assert_eq!(0, val2.page_id.unwrap());
        assert_eq!(1, val2.slot_id.unwrap());

        let p2 = sm
            .get_page(cid, 0, tid, Permissions::ReadOnly, false)
            .unwrap();
        assert_ne!(p1.get_bytes()[..], p2.get_bytes()[..]);
    }

    #[test]
    fn hs_sm_b_iter_small() {
        init();
        let sm = StorageManager::new_test_sm();
        let cid = 1;
        sm.create_table(cid);
        let tid = TransactionId::new();

        //Test one page
        let mut byte_vec: Vec<Vec<u8>> = vec![
            get_random_byte_vec(400),
            get_random_byte_vec(400),
            get_random_byte_vec(400),
        ];
        for val in &byte_vec {
            sm.insert_value(cid, val.clone(), tid);
        }
        let iter = sm.get_iterator(cid, tid, Permissions::ReadOnly);
        for (i, x) in iter.enumerate() {
            assert_eq!(byte_vec[i], x);
        }

        // Should be on two pages
        let mut byte_vec2: Vec<Vec<u8>> = vec![
            get_random_byte_vec(400),
            get_random_byte_vec(400),
            get_random_byte_vec(400),
            get_random_byte_vec(400),
        ];

        for val in &byte_vec2 {
            sm.insert_value(cid, val.clone(), tid);
        }
        byte_vec.append(&mut byte_vec2);

        let iter = sm.get_iterator(cid, tid, Permissions::ReadOnly);
        for (i, x) in iter.enumerate() {
            assert_eq!(byte_vec[i], x);
        }

        // Should be on 3 pages
        let mut byte_vec2: Vec<Vec<u8>> = vec![
            get_random_byte_vec(300),
            get_random_byte_vec(500),
            get_random_byte_vec(400),
        ];

        for val in &byte_vec2 {
            sm.insert_value(cid, val.clone(), tid);
        }
        byte_vec.append(&mut byte_vec2);

        let iter = sm.get_iterator(cid, tid, Permissions::ReadOnly);
        for (i, x) in iter.enumerate() {
            assert_eq!(byte_vec[i], x);
        }
    }

    #[test]
    #[ignore]
    fn hs_sm_b_iter_large() {
        init();
        let sm = StorageManager::new_test_sm();
        let cid = 1;

        sm.create_table(cid).unwrap();
        let tid = TransactionId::new();

        let vals = get_random_vec_of_byte_vec(1000, 40, 400);
        sm.insert_values(cid, vals, tid);
        let mut count = 0;
        for _ in sm.get_iterator(cid, tid, Permissions::ReadOnly) {
            count += 1;
        }
        assert_eq!(1000, count);
    }
}
