use std::sync::atomic::{AtomicU64, Ordering};

use mikura::{
    ObjectRecord, OverlayPatch, PropertyAcl, SchemaDescriptor, Store, OVERLAY_KIND, SCHEMA_KIND,
    SOURCE_KIND,
};

use crate::common::intern_token_budget;

static NEXT_STREAM: AtomicU64 = AtomicU64::new(1);

enum PendingOverlay {
    Absent,
    Hidden,
    Visible(OverlayPatch),
}

enum PendingSchema {
    Absent,
    Hidden,
    Visible(Box<SchemaDescriptor>),
}

struct PendingOverlayMerge {
    merged: std::collections::HashMap<String, String>,
    overlay_action: Option<String>,
    audit: Option<ObjectRecord>,
}

pub struct StreamIngest {
    bound: usize,
    id: u64,
    pending: Vec<ObjectRecord>,
    admitted: Vec<ObjectRecord>,
}

impl StreamIngest {
    /// Bound outstanding uncommitted records. `bound` must be greater than 0.
    pub fn new(bound: usize) -> Result<Self, String> {
        if bound == 0 {
            return Err("stream ingest bound must be greater than 0".into());
        }
        Ok(Self {
            bound,
            id: NEXT_STREAM.fetch_add(1, Ordering::Relaxed),
            pending: Vec::new(),
            admitted: Vec::new(),
        })
    }

    pub fn bound(&self) -> usize {
        self.bound
    }

    pub fn uncommitted(&self) -> usize {
        self.pending.len()
    }

    /// Admit `record` without writing the log or updating live maps. `load` and
    /// `evaluate` on this process stay on the last commit until [`Self::flush`].
    /// When the bound is hit, returns an error and does not accept the record
    /// (fail closed, no silent drop).
    pub fn push(&mut self, store: &mut Store, mut record: ObjectRecord) -> Result<(), String> {
        if self.pending.len() >= self.bound {
            return Err(format!(
                "stream ingest bound {} exceeded; flush before pushing more",
                self.bound
            ));
        }
        if self.pending_replay(store, &record)? {
            return Ok(());
        }
        self.pending_action(store, &record)?;
        if self.pending_schema_replace(store, &mut record)? {
            return Ok(());
        }
        let mut probe = record.clone();
        self.pending_merge(store, &mut probe, self.pending.len())?;
        if probe.action_id != record.action_id {
            self.pending_action(store, &probe)?;
        }
        self.pending_validate(&mut probe)?;
        let merge_overlay = matches!(
            self.pending_overlay(
                &record.namespace,
                &record.kind,
                &record.key,
                self.pending.len(),
            )?,
            PendingOverlay::Absent
        ) && !self.instance_is_hidden(
            store,
            &record.namespace,
            &record.kind,
            &record.key,
            self.pending.len(),
        )?;
        if merge_overlay {
            // Probe may already include a committed overlay for validation.
            // Admit the original body so Store can fence a later overlay hide
            // against the record flush will replay (ADR 0040). Keep pending
            // schema canonicalization so Action retries still match.
            probe.props = record.props.clone();
            if let PendingSchema::Visible(schema) =
                self.pending_schema(&record.namespace, &record.kind, self.pending.len())?
            {
                probe.schema_version = schema.effective_version();
                schema.canonicalize_instance(&mut probe)?;
            }
        }
        let described = if record.kind == SCHEMA_KIND {
            record.key.as_str()
        } else {
            record.kind.as_str()
        };
        let schema_follows_pending =
            match self.pending_schema(&record.namespace, described, self.pending.len())? {
                PendingSchema::Visible(_) => true,
                PendingSchema::Hidden => {
                    record.kind != SCHEMA_KIND
                        || self
                            .last_visible_pending_schema(&record.namespace, described)?
                            .is_some()
                }
                PendingSchema::Absent => {
                    record.kind == SCHEMA_KIND
                        && store.joins(&record.namespace).is_ok()
                        && store.schema(&record.namespace, described)?.is_some()
                }
            };
        if store.admit_uncommitted_buffered(
            probe.clone(),
            merge_overlay,
            schema_follows_pending,
            self.id,
        )? {
            self.admitted.push(probe);
            self.pending.push(record);
        }
        Ok(())
    }

    /// Append pending records and group-commit. Rebuild after this sees them.
    /// A durable log commit that then fails sidecar persist still clears the
    /// buffer: retrying would write a second generation (ADR 0026). A
    /// pre-commit error keeps the buffer so the clerk can retry or
    /// [`Self::discard`]. The host has no discard RPC; a permanent flush
    /// error needs process restart.
    pub fn flush(&mut self, store: &mut Store) -> Result<(), String> {
        if self.pending.is_empty() {
            return Ok(());
        }
        for record in &self.pending {
            store.reserve_intern(&record.namespace, intern_token_budget(record));
        }
        match store.append_admitted_batch(self.pending.clone()) {
            Ok(()) => {
                store.release_admitted(&self.admitted, self.id);
                self.admitted.clear();
                self.pending.clear();
                Ok(())
            }
            Err(error) if error.starts_with("committed to the log") => {
                store.release_admitted(&self.admitted, self.id);
                self.admitted.clear();
                self.pending.clear();
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    /// Drop this buffer and its Store fences without writing. A later stream
    /// on the same `Store` can then admit a conflicting schema or Action id.
    /// Dropping the value without this call keeps the fences so a retry can
    /// still flush; a durable flush of that retry releases leftover fences of
    /// the same body from dropped streams only (ADR 0040).
    pub fn discard(&mut self, store: &mut Store) {
        store.release_admitted(&self.admitted, self.id);
        self.admitted.clear();
        self.pending.clear();
    }

    /// Same Action id and body already in this buffer is a retry no-op.
    fn pending_replay(&self, store: &Store, record: &ObjectRecord) -> Result<bool, String> {
        let Some(id) = record.action_id.as_deref() else {
            return Ok(false);
        };
        for (index, pending) in self.pending.iter().enumerate() {
            if pending.action_id.as_deref() != Some(id)
                || pending.namespace != record.namespace
                || pending.kind != record.kind
                || pending.key != record.key
                || pending.hidden != record.hidden
            {
                continue;
            }
            if pending.kind == SCHEMA_KIND {
                if Self::action_body_eq(pending, record) {
                    return Ok(true);
                }
                continue;
            }
            let claimed = self.pending_claimed_body(store, pending, index)?;
            let incoming = self.pending_claimed_body(store, record, self.pending.len())?;
            if claimed.props == incoming.props {
                let mut probe = incoming;
                self.pending_validate(&mut probe)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Same Action id on a different identity or body in this buffer fails closed.
    fn pending_action(&self, store: &Store, record: &ObjectRecord) -> Result<(), String> {
        let Some(id) = record.action_id.as_deref() else {
            return Ok(());
        };
        for (index, pending) in self.pending.iter().enumerate() {
            let Some(pending_id) = pending.action_id.as_deref() else {
                continue;
            };
            if pending_id != id || pending.namespace != record.namespace {
                continue;
            }
            if pending.kind != record.kind || pending.key != record.key {
                return Err(format!(
                    "action id {id} already committed on {}/{}/{}",
                    pending.namespace, pending.kind, pending.key
                ));
            }
            if !pending.hidden && record.hidden {
                let claimed = self.pending_claimed_body(store, pending, index)?;
                if claimed.props == record.props {
                    continue;
                }
                return Err(format!("action id {id} body conflict"));
            }
            if pending.hidden != record.hidden {
                return Err(format!("action id {id} body conflict"));
            }
            if pending.kind == SCHEMA_KIND {
                if !Self::action_body_eq(pending, record) {
                    return Err(format!("action id {id} body conflict"));
                }
                continue;
            }
            let claimed = self.pending_claimed_body(store, pending, index)?;
            let incoming = self.pending_claimed_body(store, record, self.pending.len())?;
            if claimed.props != incoming.props {
                return Err(format!("action id {id} body conflict"));
            }
        }
        Ok(())
    }

    /// A later schema row in this buffer must be a valid replacement of the
    /// last pending descriptor, or flush would reject the whole batch.
    fn pending_schema_replace(
        &self,
        store: &Store,
        record: &mut ObjectRecord,
    ) -> Result<bool, String> {
        if record.kind != SCHEMA_KIND || record.hidden {
            return Ok(false);
        }
        let previous = match self.last_visible_pending_schema(&record.namespace, &record.key)? {
            Some(previous) => Some(previous),
            None if store.joins(&record.namespace).is_ok() => {
                match store.schema(&record.namespace, &record.key)? {
                    Some(previous) => Some(previous),
                    None => self.last_historical_schema(store, &record.namespace, &record.key)?,
                }
            }
            None => None,
        };
        let mut next = SchemaDescriptor::from_record(record)?;
        match previous {
            Some(previous) => {
                if self.schema_action_replay(store, record, &next)? {
                    return Ok(true);
                }
                next = previous.check_replacement(&next)?;
                self.pending_schema_migrate(store, &record.namespace, &next)?;
            }
            None => return Ok(false),
        }
        let action_id = record.action_id.clone();
        let hidden = record.hidden;
        let gen = record.gen;
        let namespace = record.namespace.clone();
        *record = next.to_record(namespace)?;
        record.action_id = action_id;
        record.hidden = hidden;
        record.gen = gen;
        Ok(false)
    }

    /// Merge the last pending overlay into `record` so admission sees the body
    /// flush will apply, without publishing it on the live maps.
    /// Overlay-merged, schema-canonical body flush will claim for `pending`.
    fn pending_claimed_body(
        &self,
        store: &Store,
        pending: &ObjectRecord,
        until: usize,
    ) -> Result<ObjectRecord, String> {
        let mut claimed = pending.clone();
        self.pending_merge(store, &mut claimed, until)?;
        if claimed.hidden {
            return Ok(claimed);
        }
        match self.pending_schema(&claimed.namespace, &claimed.kind, until)? {
            PendingSchema::Visible(schema) => schema.canonicalize_instance(&mut claimed)?,
            PendingSchema::Hidden => {}
            PendingSchema::Absent if store.joins(&claimed.namespace).is_ok() => {
                if let Some(schema) = store.schema(&claimed.namespace, &claimed.kind)? {
                    schema.canonicalize_instance(&mut claimed)?;
                }
            }
            PendingSchema::Absent => {}
        }
        Ok(claimed)
    }

    fn pending_merge(
        &self,
        store: &Store,
        record: &mut ObjectRecord,
        until: usize,
    ) -> Result<bool, String> {
        let Some(applied) = self.pending_overlay_merge(store, record, until)? else {
            return Ok(false);
        };
        record.props = applied.merged;
        if record.action_id.is_none() {
            record.action_id = applied.overlay_action;
        }
        if let Some(audit) = applied.audit {
            store.check_record_fits(&audit)?;
        }
        Ok(true)
    }

    /// Overlay merge flush would apply at `until`, plus the audit overlay it
    /// would write. Does not mutate `record`.
    fn pending_overlay_merge(
        &self,
        store: &Store,
        record: &ObjectRecord,
        until: usize,
    ) -> Result<Option<PendingOverlayMerge>, String> {
        if record.hidden
            || record.kind == SCHEMA_KIND
            || record.kind == OVERLAY_KIND
            || record.kind == SOURCE_KIND
        {
            return Ok(None);
        }
        if self.instance_is_hidden(store, &record.namespace, &record.kind, &record.key, until)? {
            return Ok(None);
        }
        let overlay = match self.pending_overlay_migrated(
            store,
            &record.namespace,
            &record.kind,
            &record.key,
            until,
        )? {
            PendingOverlay::Visible(overlay) => overlay,
            PendingOverlay::Hidden | PendingOverlay::Absent => return Ok(None),
        };
        let schema = match self.pending_schema(&record.namespace, &record.kind, until)? {
            PendingSchema::Visible(schema) => Some(*schema),
            PendingSchema::Hidden => None,
            PendingSchema::Absent if store.joins(&record.namespace).is_ok() => {
                store.schema(&record.namespace, &record.kind)?
            }
            PendingSchema::Absent => None,
        };
        let plan = schema.as_ref().map(SchemaDescriptor::conflict_plan);
        let (merged, audit) = overlay.apply_with(record.props.clone(), plan.as_ref())?;
        let audit = match audit {
            Some(audit) => Some(overlay.to_audit_record(&audit)?),
            None => None,
        };
        Ok(Some(PendingOverlayMerge {
            merged,
            overlay_action: overlay.action_id.clone(),
            audit,
        }))
    }

    /// Last generated conflict-audit overlay flush would write after `after`
    /// for this overlay identity, before later explicit overlays replace it.
    fn pending_generated_audit_after(
        &self,
        store: &Store,
        namespace: &str,
        kind: &str,
        overlay_key: &str,
        after: usize,
    ) -> Result<Option<ObjectRecord>, String> {
        let mut generated = None;
        for (index, pending) in self
            .pending
            .iter()
            .enumerate()
            .skip(after.saturating_add(1))
        {
            if pending.namespace != namespace {
                continue;
            }
            if pending.kind == OVERLAY_KIND && pending.key == overlay_key {
                generated = None;
                continue;
            }
            if pending.kind != kind
                || OverlayPatch::identity_key(&pending.kind, &pending.key) != overlay_key
            {
                continue;
            }
            if pending.hidden {
                generated = None;
                continue;
            }
            if let Some(applied) = self.pending_overlay_merge(store, pending, index)? {
                if let Some(audit) = applied.audit {
                    generated = Some(audit);
                }
            }
        }
        Ok(generated)
    }

    fn check_migrated_overlay_fits(
        store: &Store,
        patch: &OverlayPatch,
        source: &ObjectRecord,
        next: &SchemaDescriptor,
    ) -> Result<(), String> {
        let encoded = match OverlayPatch::remapped_audit(
            source,
            &next.migration.rename,
            &next.migration.drop,
        )? {
            Some(audit) => patch.to_audit_record(&audit)?,
            None => patch.to_record()?,
        };
        store.check_record_fits(&encoded)
    }

    fn pending_validate(&self, record: &mut ObjectRecord) -> Result<(), String> {
        if record.hidden
            || record.kind == SCHEMA_KIND
            || record.kind == OVERLAY_KIND
            || record.kind == SOURCE_KIND
        {
            return Ok(());
        }
        match self.pending_schema(&record.namespace, &record.kind, self.pending.len())? {
            PendingSchema::Visible(schema) => {
                record.schema_version = schema.effective_version();
                schema.canonicalize_instance(record)?;
                schema.validate(record)
            }
            PendingSchema::Hidden | PendingSchema::Absent => Ok(()),
        }
    }

    /// Fail closed on buffered overlays and instances that flush would migrate
    /// when this replacement reaches the log.
    fn pending_schema_migrate(
        &self,
        store: &Store,
        namespace: &str,
        next: &SchemaDescriptor,
    ) -> Result<(), String> {
        if next.migration.is_empty() {
            return Ok(());
        }
        let mut errors = Vec::new();
        let mut seen = Vec::new();
        for pending in self.pending.iter().rev() {
            if pending.namespace != namespace
                || pending.kind != OVERLAY_KIND
                || seen.iter().any(|key| key == &pending.key)
            {
                continue;
            }
            seen.push(pending.key.clone());
            if pending.hidden {
                continue;
            }
            let overlay_at = self
                .pending
                .iter()
                .rposition(|row| {
                    row.namespace == pending.namespace
                        && row.kind == OVERLAY_KIND
                        && row.key == pending.key
                })
                .unwrap_or(0);
            let mut patch = OverlayPatch::from_record(pending)?;
            if patch.kind != next.kind {
                continue;
            }
            if matches!(
                self.pending_overlay(namespace, &patch.kind, &patch.key, self.pending.len(),)?,
                PendingOverlay::Hidden
            ) {
                continue;
            }
            if let Err(err) = self.migrate_overlay_from(overlay_at, &mut patch, next) {
                errors.push(err);
                continue;
            }
            let source = match self.pending_generated_audit_after(
                store,
                namespace,
                &next.kind,
                &pending.key,
                overlay_at,
            ) {
                Ok(Some(audit)) => audit,
                Ok(None) => pending.clone(),
                Err(err) => {
                    errors.push(err);
                    continue;
                }
            };
            if let Err(err) = Self::check_migrated_overlay_fits(store, &patch, &source, next) {
                errors.push(err);
            }
        }
        let mut generated = std::collections::HashMap::new();
        for (index, pending) in self.pending.iter().enumerate() {
            if pending.namespace != namespace || pending.kind != next.kind {
                continue;
            }
            let overlay_key = OverlayPatch::identity_key(&pending.kind, &pending.key);
            if self.pending.iter().any(|row| {
                row.namespace == namespace && row.kind == OVERLAY_KIND && row.key == overlay_key
            }) {
                continue;
            }
            if pending.hidden {
                generated.remove(&pending.key);
                continue;
            }
            match self.pending_overlay_merge(store, pending, index) {
                Ok(Some(applied)) => {
                    if let Some(audit) = applied.audit {
                        generated.insert(pending.key.clone(), (index, audit));
                    }
                }
                Ok(None) => {}
                Err(err) => errors.push(err),
            }
        }
        for (instance_at, source) in generated.into_values() {
            let mut patch = match OverlayPatch::from_record(&source) {
                Ok(patch) => patch,
                Err(err) => {
                    errors.push(err);
                    continue;
                }
            };
            if let Err(err) = self.migrate_overlay_from(instance_at, &mut patch, next) {
                errors.push(err);
                continue;
            }
            if let Err(err) = Self::check_migrated_overlay_fits(store, &patch, &source, next) {
                errors.push(err);
            }
        }
        if !errors.is_empty() {
            return Err(format!(
                "cannot migrate overlays on {}: {}",
                next.kind,
                errors.join("; ")
            ));
        }
        if !next.migration.cast.is_empty() || !next.migration.rename.is_empty() {
            let mut errors = Vec::new();
            let mut seen = Vec::new();
            for pending in self.pending.iter().rev() {
                if pending.namespace != namespace
                    || pending.kind != next.kind
                    || seen.iter().any(|key| key == &pending.key)
                {
                    continue;
                }
                seen.push(pending.key.clone());
                let instance_at = self
                    .pending
                    .iter()
                    .rposition(|row| {
                        row.namespace == pending.namespace
                            && row.kind == pending.kind
                            && row.key == pending.key
                    })
                    .unwrap_or(0);
                let mut probe = pending.clone();
                self.pending_merge(store, &mut probe, instance_at)?;
                let mut props = probe.props;
                if let Err(err) =
                    self.migrate_instance_from(instance_at, namespace, next, &mut props)
                {
                    errors.push(format!(
                        "{namespace}/{}/{} {err}",
                        pending.kind, pending.key
                    ));
                }
            }
            if !errors.is_empty() {
                return Err(format!(
                    "cannot migrate instances on {}: {}",
                    next.kind,
                    errors.join("; ")
                ));
            }
        }
        let mut steps = Vec::new();
        for pending in &self.pending {
            if pending.namespace != namespace
                || pending.kind != SCHEMA_KIND
                || pending.key != next.kind
                || pending.hidden
            {
                continue;
            }
            steps.push(SchemaDescriptor::from_record(pending)?);
        }
        steps.push(next.clone());
        let mut skip_overlay_keys = Vec::new();
        let mut skip_instance_keys = Vec::new();
        for pending in &self.pending {
            if pending.namespace != namespace {
                continue;
            }
            if pending.kind == OVERLAY_KIND {
                skip_overlay_keys.push(pending.key.clone());
            }
            if pending.kind == next.kind {
                skip_instance_keys.push(pending.key.clone());
                if pending.hidden {
                    skip_overlay_keys.push(OverlayPatch::identity_key(&pending.kind, &pending.key));
                }
            }
        }
        if store.joins(namespace).is_ok() {
            store.preflight_schema_chain(
                namespace,
                &steps,
                &skip_overlay_keys,
                &skip_instance_keys,
            )?;
        }
        Ok(())
    }

    fn pending_schema(
        &self,
        namespace: &str,
        kind: &str,
        until: usize,
    ) -> Result<PendingSchema, String> {
        for pending in self.pending_until(until).iter().rev() {
            if pending.namespace != namespace || pending.kind != SCHEMA_KIND || pending.key != kind
            {
                continue;
            }
            if pending.hidden {
                return Ok(PendingSchema::Hidden);
            }
            return SchemaDescriptor::from_record(pending)
                .map(Box::new)
                .map(PendingSchema::Visible);
        }
        Ok(PendingSchema::Absent)
    }

    fn last_visible_pending_schema(
        &self,
        namespace: &str,
        kind: &str,
    ) -> Result<Option<SchemaDescriptor>, String> {
        for pending in self.pending.iter().rev() {
            if pending.namespace != namespace || pending.kind != SCHEMA_KIND || pending.key != kind
            {
                continue;
            }
            if pending.hidden {
                continue;
            }
            return SchemaDescriptor::from_record(pending).map(Some);
        }
        Ok(None)
    }

    fn migrate_overlay_from(
        &self,
        after: usize,
        patch: &mut OverlayPatch,
        next: &SchemaDescriptor,
    ) -> Result<(), String> {
        for pending in &self.pending[after.saturating_add(1)..] {
            if pending.namespace != patch.namespace
                || pending.kind != SCHEMA_KIND
                || pending.key != next.kind
                || pending.hidden
            {
                continue;
            }
            let schema = SchemaDescriptor::from_record(pending)?;
            if schema.migration.is_empty() {
                continue;
            }
            Self::apply_overlay_migration(patch, &schema)?;
        }
        Self::apply_overlay_migration(patch, next)
    }

    fn migrate_instance_from(
        &self,
        after: usize,
        namespace: &str,
        next: &SchemaDescriptor,
        props: &mut std::collections::HashMap<String, String>,
    ) -> Result<(), String> {
        for pending in &self.pending[after.saturating_add(1)..] {
            if pending.namespace != namespace
                || pending.kind != SCHEMA_KIND
                || pending.key != next.kind
                || pending.hidden
            {
                continue;
            }
            let schema = SchemaDescriptor::from_record(pending)?;
            if schema.migration.cast.is_empty() && schema.migration.rename.is_empty() {
                continue;
            }
            schema.migration.apply(props, false)?;
        }
        next.migration.apply(props, false)
    }

    fn action_body_eq(left: &ObjectRecord, right: &ObjectRecord) -> bool {
        if left.kind != SCHEMA_KIND {
            return left.props == right.props;
        }
        let Ok(mut incoming) = SchemaDescriptor::from_record(right) else {
            return false;
        };
        let Ok(mut stored) = SchemaDescriptor::from_record(left) else {
            return left.props == right.props;
        };
        // Unspecified (0) on a replacement is previous+1, not effective 1.
        if incoming.version == 0 {
            incoming.version = stored.effective_version();
        }
        stored.version = stored.effective_version();
        incoming == stored
    }

    fn schema_action_replay(
        &self,
        store: &Store,
        record: &ObjectRecord,
        next: &SchemaDescriptor,
    ) -> Result<bool, String> {
        let Some(id) = record.action_id.as_deref() else {
            return Ok(false);
        };
        for pending in &self.pending {
            if pending.action_id.as_deref() != Some(id)
                || pending.namespace != record.namespace
                || pending.kind != SCHEMA_KIND
                || pending.key != record.key
                || pending.hidden
            {
                continue;
            }
            if Self::schema_descriptor_replay(pending, next) {
                return Ok(true);
            }
        }
        if store.joins(&record.namespace).is_err() {
            return Ok(false);
        }
        match store.history(
            &record.namespace,
            SCHEMA_KIND,
            &record.key,
            &PropertyAcl::allow_all(),
        ) {
            Ok(versions) => {
                for version in versions {
                    if version.record.action_id.as_deref() != Some(id) {
                        continue;
                    }
                    if Self::schema_descriptor_replay(&version.record, next) {
                        return Ok(true);
                    }
                }
            }
            Err(err) if err.contains("unknown identity") => {}
            Err(err) => return Err(err),
        }
        Ok(false)
    }

    fn schema_descriptor_replay(stored: &ObjectRecord, incoming: &SchemaDescriptor) -> bool {
        let Ok(mut stored) = SchemaDescriptor::from_record(stored) else {
            return false;
        };
        let mut probe = incoming.clone();
        if probe.version == 0 {
            probe.version = stored.effective_version();
        }
        stored.version = stored.effective_version();
        probe == stored
    }

    fn apply_overlay_migration(
        patch: &mut OverlayPatch,
        next: &SchemaDescriptor,
    ) -> Result<(), String> {
        let ident = format!("{}/{}/{}", patch.namespace, next.kind, patch.key);
        let mut errors = Vec::new();
        for name in patch.props.keys().chain(patch.cleared.iter()) {
            if next.migration.drop.iter().any(|dropped| dropped == name) {
                errors.push(format!("overlay {ident} cannot carry dropped {name}"));
            }
        }
        if errors.is_empty() {
            if let Err(err) = next.migration.apply(&mut patch.props, false) {
                errors.push(format!("overlay {ident} {err}"));
            }
        }
        if errors.is_empty() {
            let mut cleared = Vec::new();
            for name in &patch.cleared {
                let renamed = next
                    .migration
                    .rename
                    .iter()
                    .find(|(old, _)| old == name)
                    .map(|(_, new)| new.clone())
                    .unwrap_or_else(|| name.clone());
                if !cleared.contains(&renamed) {
                    cleared.push(renamed);
                }
            }
            patch.cleared = cleared;
            for name in patch.props.keys().chain(patch.cleared.iter()) {
                if !next.properties.iter().any(|property| property == name) {
                    errors.push(format!("overlay {ident} cannot carry {name}"));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn pending_overlay_migrated(
        &self,
        store: &Store,
        namespace: &str,
        kind: &str,
        key: &str,
        until: usize,
    ) -> Result<PendingOverlay, String> {
        let overlay_key = OverlayPatch::identity_key(kind, key);
        let mut found_at = None;
        let mut overlay = PendingOverlay::Absent;
        for (index, pending) in self.pending_until(until).iter().enumerate() {
            if pending.namespace != namespace {
                continue;
            }
            if pending.kind == OVERLAY_KIND && pending.key == overlay_key {
                found_at = Some(index);
                overlay = if pending.hidden {
                    PendingOverlay::Hidden
                } else {
                    OverlayPatch::from_record(pending).map(PendingOverlay::Visible)?
                };
                continue;
            }
            if pending.kind == kind && pending.key == key && pending.hidden {
                found_at = Some(index);
                overlay = PendingOverlay::Hidden;
            }
        }
        if matches!(overlay, PendingOverlay::Absent) && store.joins(namespace).is_ok() {
            if let Some(committed) = store.overlay(namespace, kind, key)? {
                overlay = PendingOverlay::Visible(committed);
            }
        }
        let PendingOverlay::Visible(mut patch) = overlay else {
            return Ok(overlay);
        };
        let later = found_at.map(|index| index + 1).unwrap_or(0);
        for pending in &self.pending_until(until)[later..] {
            if pending.namespace != namespace || pending.kind != SCHEMA_KIND || pending.key != kind
            {
                continue;
            }
            if pending.hidden {
                continue;
            }
            let schema = SchemaDescriptor::from_record(pending)?;
            if schema.migration.is_empty() {
                continue;
            }
            Self::apply_overlay_migration(&mut patch, &schema)?;
        }
        Ok(PendingOverlay::Visible(patch))
    }

    fn pending_overlay(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        until: usize,
    ) -> Result<PendingOverlay, String> {
        let overlay_key = OverlayPatch::identity_key(kind, key);
        for pending in self.pending_until(until).iter().rev() {
            if pending.namespace != namespace {
                continue;
            }
            if pending.kind == OVERLAY_KIND && pending.key == overlay_key {
                if pending.hidden {
                    return Ok(PendingOverlay::Hidden);
                }
                return OverlayPatch::from_record(pending).map(PendingOverlay::Visible);
            }
            if pending.kind == kind && pending.key == key && pending.hidden {
                return Ok(PendingOverlay::Hidden);
            }
        }
        Ok(PendingOverlay::Absent)
    }

    fn instance_is_hidden(
        &self,
        store: &Store,
        namespace: &str,
        kind: &str,
        key: &str,
        until: usize,
    ) -> Result<bool, String> {
        if let Some(pending) = self.pending_until(until).iter().rev().find(|pending| {
            pending.namespace == namespace && pending.kind == kind && pending.key == key
        }) {
            return Ok(pending.hidden);
        }
        if store.joins(namespace).is_err() {
            return Ok(false);
        }
        match store.load(namespace, kind, key, &PropertyAcl::allow_all()) {
            Ok(live) => Ok(live.hidden),
            Err(_) => Ok(false),
        }
    }

    fn last_historical_schema(
        &self,
        store: &Store,
        namespace: &str,
        kind: &str,
    ) -> Result<Option<SchemaDescriptor>, String> {
        match store.history(namespace, SCHEMA_KIND, kind, &PropertyAcl::allow_all()) {
            Ok(versions) => versions
                .last()
                .map(|version| SchemaDescriptor::from_record(&version.record))
                .transpose(),
            Err(err) if err.contains("unknown identity") => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn pending_until(&self, until: usize) -> &[ObjectRecord] {
        let end = until.min(self.pending.len());
        &self.pending[..end]
    }
}

impl Drop for StreamIngest {
    fn drop(&mut self) {
        // Keep Store fences so a later StreamIngest can retry the same body
        // (ADR 0040). discard, a matching flush, or reopen releases them.
        if !self.admitted.is_empty() {
            Store::abandon_stream_owner(self.id);
        }
    }
}
