use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::actions::Action;
use crate::log::{LogWriter, SyncPolicy, read_records};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectRecord {
    pub gen: u64,
    pub kind: String,
    pub key: String,
    pub hidden: bool,
    pub props: HashMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JoinMaps {
    pub visible_customers: HashSet<String>,
    pub order_customer: HashMap<String, String>,
    pub shipment_order_amount: HashMap<String, (String, i64)>,
}

impl JoinMaps {
    pub fn hop_count(&self) -> usize {
        let mut reachable = HashSet::new();
        for (order_id, _) in self.shipment_order_amount.values() {
            if let Some(customer_id) = self.order_customer.get(order_id) {
                if self.visible_customers.contains(customer_id) {
                    reachable.insert(customer_id.clone());
                }
            }
        }
        reachable.len()
    }

    pub fn sum_amount(&self) -> i64 {
        let mut total = 0i64;
        for (order_id, amount) in self.shipment_order_amount.values() {
            if let Some(customer_id) = self.order_customer.get(order_id) {
                if self.visible_customers.contains(customer_id) {
                    total += amount;
                }
            }
        }
        total
    }
}

pub struct Store {
    log: PathBuf,
    writer: LogWriter,
    objects: HashMap<(String, String), ObjectRecord>,
    joins: JoinMaps,
}

impl Store {
    pub fn create(log: &Path) -> Result<Self, String> {
        Self::create_with_sync(log, SyncPolicy::Group(32))
    }

    pub fn create_with_sync(log: &Path, sync: SyncPolicy) -> Result<Self, String> {
        Ok(Self {
            log: log.to_path_buf(),
            writer: LogWriter::create(log, sync)?,
            objects: HashMap::new(),
            joins: JoinMaps::default(),
        })
    }

    pub fn open(log: &Path) -> Result<Self, String> {
        Self::open_with_sync(log, SyncPolicy::Group(32))
    }

    pub fn open_with_sync(log: &Path, sync: SyncPolicy) -> Result<Self, String> {
        let mut store = Self {
            log: log.to_path_buf(),
            writer: LogWriter::open(log, sync)?,
            objects: HashMap::new(),
            joins: JoinMaps::default(),
        };
        for record in read_records(log)? {
            store.apply_record(record);
        }
        Ok(store)
    }

    pub fn apply_record(&mut self, record: ObjectRecord) {
        let id = (record.kind.clone(), record.key.clone());
        if let Some(old) = self.objects.remove(&id) {
            self.unindex(&old);
        }
        self.index(&record);
        self.objects.insert(id, record);
    }

    pub fn append(&mut self, mut record: ObjectRecord) -> Result<(), String> {
        let id = (record.kind.clone(), record.key.clone());
        if let Some(existing) = self.objects.get(&id) {
            record.gen = existing.gen.max(1) + 1;
        } else if record.gen == 0 {
            record.gen = 1;
        }
        self.writer.append_record(&record)?;
        self.writer.flush()?;
        self.apply_record(record);
        Ok(())
    }

    pub fn apply_action(&mut self, action: Action) -> Result<(), String> {
        self.append(ObjectRecord {
            gen: 0,
            kind: action.kind,
            key: action.key,
            hidden: false,
            props: action.props,
        })
    }

    pub fn joins(&self) -> &JoinMaps {
        &self.joins
    }

    pub fn visible_of_kind(&self, kind: &str) -> Vec<&ObjectRecord> {
        self.objects
            .values()
            .filter(|record| record.kind == kind && !record.hidden)
            .collect()
    }

    pub fn replace_kind(&mut self, kind: &str, records: Vec<ObjectRecord>) -> Result<(), String> {
        let keep: Vec<ObjectRecord> = self
            .objects
            .values()
            .filter(|record| record.kind != kind)
            .cloned()
            .collect();
        self.writer = LogWriter::create(&self.log, SyncPolicy::Group(32))?;
        self.objects.clear();
        self.joins = JoinMaps::default();
        for record in keep.into_iter().chain(records) {
            self.append(record)?;
        }
        Ok(())
    }

    fn unindex(&mut self, record: &ObjectRecord) {
        match record.kind.as_str() {
            "Customer" => {
                self.joins.visible_customers.remove(&record.key);
            }
            "Order" => {
                self.joins.order_customer.remove(&record.key);
            }
            "Shipment" => {
                self.joins.shipment_order_amount.remove(&record.key);
            }
            _ => {}
        }
    }

    fn index(&mut self, record: &ObjectRecord) {
        if record.hidden {
            return;
        }
        match record.kind.as_str() {
            "Customer" => {
                self.joins.visible_customers.insert(record.key.clone());
            }
            "Order" => {
                if let Some(customer_id) = record.props.get("customer_id") {
                    self.joins
                        .order_customer
                        .insert(record.key.clone(), customer_id.clone());
                }
            }
            "Shipment" => {
                if let (Some(order_id), Some(amount)) = (
                    record.props.get("order_id"),
                    record.props.get("amount").and_then(|raw| raw.parse().ok()),
                ) {
                    self.joins
                        .shipment_order_amount
                        .insert(record.key.clone(), (order_id.clone(), amount));
                }
            }
            _ => {}
        }
    }
}
