use crate::{Record, State};
use std::{error::Error, ops::Bound};

type StoreError = Box<dyn Error + Send + Sync>;

pub struct Store {
    backend: Backend,
}

enum Backend {
    Memory(Box<State>),
    Turso {
        connection: turso::Connection,
        last_record: u64,
    },
}

impl Store {
    pub fn memory() -> Self {
        Self {
            backend: Backend::Memory(Box::default()),
        }
    }

    pub async fn open(path: &str) -> Result<Self, StoreError> {
        let db = turso::Builder::new_local(path).build().await?;
        let connection = db.connect()?;
        crate::migrations::apply(&connection).await?;
        Ok(Self {
            backend: Backend::Turso {
                connection,
                last_record: 0,
            },
        })
    }

    pub async fn load(&mut self) -> Result<State, StoreError> {
        let (connection, last_record) = match &mut self.backend {
            Backend::Memory(state) => return Ok((**state).clone()),
            Backend::Turso {
                connection,
                last_record,
            } => (connection, last_record),
        };
        let mut state = {
            let mut rows = connection
                .query("SELECT payload FROM apron_state WHERE id = 1", ())
                .await?;
            match rows.next().await? {
                Some(row) => serde_json::from_str(&row.get::<String>(0)?)?,
                None => State::default(),
            }
        };
        let mut records = connection
            .query("SELECT id, payload FROM apron_log ORDER BY id", ())
            .await?;
        *last_record = 0;
        while let Some(row) = records.next().await? {
            let id = u64::try_from(row.get::<i64>(0)?)?;
            let record: Record = serde_json::from_str(&row.get::<String>(1)?)?;
            state.records.insert(id, record);
            *last_record = id;
        }
        state.rebuild_room_index();
        Ok(state)
    }

    pub async fn commit(&mut self, state: &State) -> Result<(), StoreError> {
        let (connection, last_record) = match &mut self.backend {
            Backend::Memory(committed) => {
                **committed = state.clone();
                return Ok(());
            }
            Backend::Turso {
                connection,
                last_record,
            } => (connection, last_record),
        };
        let snapshot = serde_json::to_string(state)?;
        let transaction = connection.transaction().await?;
        let mut committed_through = *last_record;
        for (&id, record) in state
            .records
            .range((Bound::Excluded(*last_record), Bound::Unbounded))
        {
            transaction
                .execute(
                    "INSERT INTO apron_log VALUES (?1, ?2)",
                    turso::params![i64::try_from(id)?, serde_json::to_string(record)?],
                )
                .await?;
            committed_through = id;
        }
        transaction.execute(
            "INSERT INTO apron_state VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
            [snapshot],
        ).await?;
        transaction.commit().await?;
        *last_record = committed_through;
        Ok(())
    }
}
