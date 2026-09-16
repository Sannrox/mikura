use mikura::{ObjectRecord, Store};

pub struct StreamIngest {
    bound: usize,
    uncommitted: usize,
}

impl StreamIngest {
    /// Bound outstanding uncommitted records. `bound` must be greater than 0.
    pub fn new(bound: usize) -> Result<Self, String> {
        if bound == 0 {
            return Err("stream ingest bound must be greater than 0".into());
        }
        Ok(Self {
            bound,
            uncommitted: 0,
        })
    }

    pub fn bound(&self) -> usize {
        self.bound
    }

    pub fn uncommitted(&self) -> usize {
        self.uncommitted
    }

    /// Append without `commit`. Live maps update immediately. When the bound is
    /// hit, returns an error and does not append (fail closed, no silent drop).
    pub fn push(&mut self, store: &mut Store, record: ObjectRecord) -> Result<(), String> {
        if self.uncommitted >= self.bound {
            return Err(format!(
                "stream ingest bound {} exceeded; flush before pushing more",
                self.bound
            ));
        }
        store.append_uncommitted(record)?;
        self.uncommitted += 1;
        Ok(())
    }

    /// Group-commit uncommitted records. Rebuild after this sees them.
    pub fn flush(&mut self, store: &mut Store) -> Result<(), String> {
        store.commit()?;
        self.uncommitted = 0;
        Ok(())
    }
}
