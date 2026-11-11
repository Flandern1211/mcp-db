import { Database, type SQLQueryBindings } from "bun:sqlite";
import type {
  Driver,
  DriverName,
  PoolRole,
  QueryResult,
  TableColumn,
  TableDescription,
  TableForeignKey,
  TableIndex,
  TableRef,
} from "./types.ts";

interface ColumnRow {
  name: string;
  type: string;
  notnull: number;
  dflt_value: string | null;
  pk: number;
}

interface IndexListRow {
  name: string;
  unique: number;
  origin: string;
  partial: number;
}

interface IndexInfoRow {
  seqno: number;
  cid: number;
  name: string;
}

interface ForeignKeyRow {
  id: number;
  seq: number;
  table: string;
  from: string;
  to: string;
  on_update: string;
  on_delete: string;
  match: string;
}

interface ExplainRow {
  id: number;
  parent: number;
  notused: number;
  detail: string;
}

interface TableRow {
  schema: string;
  name: string;
  type: string;
}

function parseDsn(dsn: string): { path: string; readonly: boolean } {
  let rest = dsn;
  if (rest.startsWith("file:")) rest = rest.slice("file:".length);
  const queryIndex = rest.indexOf("?");
  const path = queryIndex === -1 ? rest : rest.slice(0, queryIndex);
  const query = new URLSearchParams(queryIndex === -1 ? "" : rest.slice(queryIndex + 1));
  return { path, readonly: query.get("mode") === "ro" };
}

function quoteIdentifier(name: string): string {
  return `"${name.replace(/"/g, '""')}"`;
}

export class SqliteDriver implements Driver {
  readonly driver: DriverName = "sqlite";
  readonly role: PoolRole;
  private readonly db: Database;

  constructor(dsn: string, role: PoolRole) {
    this.role = role;
    const { path, readonly } = parseDsn(dsn);
    this.db = new Database(path, readonly ? { readonly: true } : { create: true });
  }

  async init(): Promise<void> {
    this.db.query("SELECT 1").get();
  }

  async ping(): Promise<boolean> {
    try {
      this.db.query("SELECT 1").get();
      return true;
    } catch {
      return false;
    }
  }

  async listTables(): Promise<TableRef[]> {
    const rows = this.db
      .query(
        `SELECT 'main' AS schema, name, type
         FROM sqlite_master
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%'
         ORDER BY name`,
      )
      .all() as TableRow[];
    return rows;
  }

  async describeTable(table: string): Promise<TableDescription> {
    const columns = this.db
      .query(`PRAGMA table_info(${quoteIdentifier(table)})`)
      .all() as ColumnRow[];

    const indexList = this.db
      .query(`PRAGMA index_list(${quoteIdentifier(table)})`)
      .all() as IndexListRow[];

    const indexes: TableIndex[] = indexList.map((index) => {
      const info = this.db
        .query(`PRAGMA index_info(${quoteIdentifier(index.name)})`)
        .all() as IndexInfoRow[];
      return {
        name: index.name,
        unique: Boolean(index.unique),
        columns: info.map((row) => row.name),
      };
    });

    const fkRows = this.db
      .query(`PRAGMA foreign_key_list(${quoteIdentifier(table)})`)
      .all() as ForeignKeyRow[];

    const fkMap = new Map<number, TableForeignKey>();
    for (const row of fkRows) {
      let fk = fkMap.get(row.id);
      if (!fk) {
        fk = { name: `fk_${row.id}`, columns: [], referencedTable: row.table, referencedColumns: [] };
        fkMap.set(row.id, fk);
      }
      fk.columns.push(row.from);
      fk.referencedColumns.push(row.to);
    }

    return {
      schema: "main",
      table,
      columns: columns.map(
        (row): TableColumn => ({
          name: row.name,
          type: row.type,
          nullable: row.notnull === 0,
          default: row.dflt_value,
          primaryKey: row.pk > 0,
        }),
      ),
      indexes,
      foreignKeys: [...fkMap.values()],
    };
  }

  async explain(sql: string, params: unknown[]): Promise<string> {
    const rows = this.db
      .query(`EXPLAIN QUERY PLAN ${sql}`)
      .all(...(params as SQLQueryBindings[])) as ExplainRow[];
    return rows.map((row) => row.detail).join("\n");
  }

  async execute(sql: string, params: unknown[]): Promise<QueryResult> {
    const started = performance.now();
    const statement = this.db.prepare(sql);
    const bindings = params as SQLQueryBindings[];

    if (statement.columnNames.length > 0) {
      const rows = statement.all(...bindings) as Record<string, unknown>[];
      return {
        columns: statement.columnNames,
        rows,
        rowCount: rows.length,
        durationMs: Math.round(performance.now() - started),
      };
    }

    const info = statement.run(...bindings);
    return {
      columns: [],
      rows: [],
      rowCount: info.changes,
      durationMs: Math.round(performance.now() - started),
    };
  }

  async close(): Promise<void> {
    this.db.close();
  }
}