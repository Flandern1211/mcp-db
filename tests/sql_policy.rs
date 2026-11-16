use mcp_db::{validate, validate_for_tool, Error, SqlClass, Tool};

#[test]
fn read_queries_are_classified_as_query() {
    for sql in [
        "SELECT * FROM users",
        "WITH recent AS (SELECT * FROM users) SELECT * FROM recent",
        "SELECT 1 UNION SELECT 2",
        "VALUES (1)",
    ] {
        let validated = validate(sql).expect(sql);
        assert_eq!(validated.class, SqlClass::Query, "{sql}");
        assert!(!validated.high_risk);
    }
}

#[test]
fn read_only_bypasses_are_rejected() {
    for sql in [
        "SELECT * INTO copied_users FROM users",
        "SELECT * FROM users FOR UPDATE",
        "WITH removed AS (DELETE FROM users RETURNING *) SELECT * FROM removed",
        "EXPLAIN ANALYZE SELECT 1",
        "EXPLAIN SELECT 1",
        "SELECT 1; SELECT 2",
    ] {
        assert!(matches!(validate(sql), Err(Error::SqlRejected(_))), "{sql}");
    }
}

#[test]
fn writes_are_classified_and_missing_where_is_high_risk() {
    let insert = validate("INSERT INTO users (id, name) VALUES (2, 'bob')").unwrap();
    assert_eq!(insert.class, SqlClass::Mutate);
    assert!(!insert.high_risk);

    let update = validate("UPDATE users SET name = 'bob'").unwrap();
    assert_eq!(update.class, SqlClass::Mutate);
    assert!(update.high_risk);

    let scoped_update = validate("UPDATE users SET name = 'bob' WHERE id = 2").unwrap();
    assert!(!scoped_update.high_risk);

    let delete = validate("DELETE FROM users").unwrap();
    assert!(delete.high_risk);

    let truncate = validate("TRUNCATE users").unwrap();
    assert_eq!(truncate.class, SqlClass::Mutate);
    assert!(truncate.high_risk);
}

#[test]
fn local_ddl_is_classified_and_destructive_ddl_is_high_risk() {
    let create = validate("CREATE TABLE logs (id bigint)").unwrap();
    assert_eq!(create.class, SqlClass::Ddl);
    assert!(!create.high_risk);

    let create_index = validate("CREATE INDEX users_name ON users (name)").unwrap();
    assert_eq!(create_index.class, SqlClass::Ddl);

    let alter = validate("ALTER TABLE users ADD COLUMN active boolean").unwrap();
    assert_eq!(alter.class, SqlClass::Ddl);
    assert!(alter.high_risk);

    let drop = validate("DROP TABLE users").unwrap();
    assert_eq!(drop.class, SqlClass::Ddl);
    assert!(drop.high_risk);
}

#[test]
fn global_and_administrative_statements_are_rejected() {
    for sql in [
        "CREATE DATABASE app",
        "DROP DATABASE app",
        "CREATE SCHEMA private",
        "DROP SCHEMA private",
        "CREATE ROLE reader",
        "ALTER ROLE reader WITH LOGIN",
        "GRANT SELECT ON users TO reader",
        "REVOKE SELECT ON users FROM reader",
        "CALL do_something()",
        "COPY users TO STDOUT",
        "MERGE INTO users u USING staging s ON u.id = s.id WHEN MATCHED THEN DELETE",
        "SET search_path = public",
        "USE app",
    ] {
        assert!(matches!(validate(sql), Err(Error::SqlRejected(_))), "{sql}");
    }
}

#[test]
fn cross_tool_calls_are_rejected() {
    assert!(matches!(
        validate_for_tool("CREATE TABLE t (id bigint)", Tool::QuerySql),
        Err(Error::SqlRejected(_))
    ));
    assert!(matches!(
        validate_for_tool("SELECT * FROM users", Tool::MutateSql),
        Err(Error::SqlRejected(_))
    ));
    assert!(matches!(
        validate_for_tool("INSERT INTO users (id) VALUES (1)", Tool::ExecuteDdl),
        Err(Error::SqlRejected(_))
    ));
    assert!(matches!(
        validate_for_tool("SELECT 1", Tool::ListSources),
        Err(Error::SqlRejected(_))
    ));
}

#[test]
fn explain_sql_is_generated_from_the_validated_statement() {
    let validated = validate_for_tool("SELECT * FROM users;", Tool::ExplainQuery).unwrap();
    assert_eq!(
        validated.explain_sql(),
        "EXPLAIN (FORMAT JSON) SELECT * FROM users"
    );
}
