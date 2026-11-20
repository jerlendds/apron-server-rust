pub(crate) async fn apply(connection: &turso::Connection) -> Result<(), turso::Error> {
    connection.execute(
            "CREATE TABLE IF NOT EXISTS apron_state (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)",
            (),
        ).await?;
    connection
        .execute(
            "CREATE TABLE IF NOT EXISTS apron_log (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)",
            (),
        )
        .await?;
    Ok(())
}
