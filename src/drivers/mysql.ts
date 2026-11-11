import mysql from "mysql2/promise";
import type { FieldPacket, Pool, ResultSetHeader, RowDataPacket } from "mysql2/promise";
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

interface ColumnRow extends RowDataPacket {
  name: string;
  type: string;
  nullable: number;
  default: string | null;
  primaryKey: number;
}

interface IndexRow extends RowDataPacket {
  name: string;
  unique: number;
  column: string;
  seq: number;
}

interface ForeignKeyRow extends RowDataPacket {
  name: string;
  column: string;
  referenced_table: string;
  referenced_column: string;
  ordinal: number;
}

interface TableRow extends RowDataPacket {
  schema: string;
  name: string;
  type: string;
}

export class MysqlDriver implements Driver {
  readonly driver: DriverName = "mysql";
  readonly role: PoolRole;
  private readonly pool: Pool;
  private readonly timeoutMs?: number;

  constructor(dsn: string, role: PoolRole, statementTimeoutMs?: number) {
    this.role = role;
    this.timeoutMs = statementTimeoutMs;
    this.pool = mysql.createPool(dsn);
  }

  async init(): Promise<void> {
    const connection = await this.pool.getConnection();
    connection.release();
  }

  async ping(): Promise<boolean> {
    try {
      await this.pool.query("SELECT 1");
      return true;
    } catch {
      return false;
    }
  }

  private async query(sql: string, params: unknown[] = []): Promise<[unknown, FieldPacket[]]> {
    return this.pool.query({ sql, values: params, timeout: this.timeoutMs }) as Promise<
      [unknown, FieldPacket[]]
    >;
  }

  async listTables(schema?: string): Promise<TableRef[]> {
    const [rows] = await this.query(
      `SELECT table_schema AS \`schema\`, table_name AS name, table_type AS type
       FROM information_schema.tables
       WHERE table_schema NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys')
         AND (? IS NULL OR table_schema = ?)
       ORDER BY 1, 2`,
      [schema ?? null, schema ?? null],
    );
    return (rows as TableRow[]).map((row) => ({ schema: row.schema, name: row.name, type: row.type }));
  }

  async describeTable(table: string, schema?: string): Promise<TableDescription> {
    const [columns] = await this.query(
      `SELECT column_name AS name,
              column_type AS type,
              (is_nullable = 'YES') AS nullable,
              column_default AS \`default\`,
              (column_key = 'PRI') AS primaryKey
       FROM information_schema.columns
       WHERE table_name = ? AND table_schema = COALESCE(?, DATABASE())
       ORDER BY ordinal_position`,
      [table, schema ?? null],
    );

    const [indexRows] = await this.query(
      `SELECT index_name AS name,
              (non_unique = 0) AS \`unique\`,
              column_name AS \`column\`,
              seq_in_index AS seq
       FROM information_schema.statistics
       WHERE table_name = ? AND table_schema = COALESCE(?, DATABASE())
       ORDER BY index_name, seq_in_index`,
      [table, schema ?? null],
    );

    const [fkRows] = await this.query(
      `SELECT constraint_name AS name,
              column_name AS \`column\`,
              referenced_table_name AS referenced_table,
              referenced_column_name AS referenced_column,
              ordinal_position AS ordinal
       FROM information_schema.key_column_usage
       WHERE table_name = ?
         AND table_schema = COALESCE(?, DATABASE())
         AND referenced_table_name IS NOT NULL
       ORDER BY constraint_name, ordinal_position`,
      [table, schema ?? null],
    );

    const indexes: TableIndex[] = [];
    for (const row of indexRows as IndexRow[]) {
      let index = indexes.find((item) => item.name === row.name);
      if (!index) {
        index = { name: row.name, unique: Boolean(row.unique), columns: [] };
        indexes.push(index);
      }
      index.columns.push(row.column);
    }

    const fkMap = new Map<string, TableForeignKey>();
    for (const row of fkRows as ForeignKeyRow[]) {
      let fk = fkMap.get(row.name);
      if (!fk) {
        fk = {
          name: row.name,
          columns: [],
          referencedTable: row.referenced_table,
          referencedColumns: [],
        };
        fkMap.set(row.name, fk);
      }
      fk.columns.push(row.column);
      fk.referencedColumns.push(row.referenced_column);
    }

    return {
      schema: schema ?? "default",
      table,
      columns: (columns as ColumnRow[]).map(
        (row): TableColumn => ({
          name: row.name,
          type: row.type,
          nullable: Boolean(row.nullable),
          default: row.default,
          primaryKey: Boolean(row.primaryKey),
        }),
      ),
      indexes,
      foreignKeys: [...fkMap.values()],
    };
  }

  async explain(sql: string, params: unknown[]): Promise<string> {
    const [rows] = await this.query(`EXPLAIN ${sql}`, params);
    return JSON.stringify(rows, null, 2);
  }

  async execute(sql: string, params: unknown[]): Promise<QueryResult> {
    const started = performance.now();
    const [rows, fields] = await this.query(sql, params);
    const isRows = Array.isArray(rows);
    return {
      columns: isRows ? ((fields as FieldPacket[] | undefined) ?? []).map((field) => field.name) : [],
      rows: isRows ? (rows as Record<string, unknown>[]) : [],
      rowCount: isRows ? rows.length : ((rows as ResultSetHeader).affectedRows ?? 0),
      durationMs: Math.round(performance.now() - started),
    };
  }

  async close(): Promise<void> {
    await this.pool.end();
  }
}