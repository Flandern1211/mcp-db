import pg from "pg";
import type { Pool as PgPool } from "pg";
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

const { Pool } = pg;

interface TableRow {
  schema: string;
  name: string;
  type: string;
}

interface ColumnRow {
  name: string;
  type: string;
  nullable: boolean;
  default: string | null;
  primaryKey: boolean;
}

interface IndexRow {
  name: string;
  unique: boolean;
  columns: string[];
}

interface ForeignKeyRow {
  name: string;
  column: string;
  referenced_table: string;
  referenced_column: string;
}

function groupIndexes(rows: IndexRow[]): TableIndex[] {
  return rows.map((row) => ({ name: row.name, unique: row.unique, columns: row.columns }));
}

function groupForeignKeys(rows: ForeignKeyRow[]): TableForeignKey[] {
  const map = new Map<string, TableForeignKey>();
  for (const row of rows) {
    let fk = map.get(row.name);
    if (!fk) {
      fk = { name: row.name, columns: [], referencedTable: row.referenced_table, referencedColumns: [] };
      map.set(row.name, fk);
    }
    fk.columns.push(row.column);
    fk.referencedColumns.push(row.referenced_column);
  }
  return [...map.values()];
}

export class PostgresDriver implements Driver {
  readonly driver: DriverName = "postgres";
  readonly role: PoolRole;
  private readonly pool: PgPool;

  constructor(dsn: string, role: PoolRole, statementTimeoutMs?: number) {
    this.role = role;
    this.pool = new Pool({
      connectionString: dsn,
      max: 5,
      connectionTimeoutMillis: 5_000,
      ...(statementTimeoutMs ? { statement_timeout: statementTimeoutMs } : {}),
    });
  }

  async init(): Promise<void> {
    const client = await this.pool.connect();
    client.release();
  }

  async ping(): Promise<boolean> {
    try {
      await this.pool.query("SELECT 1");
      return true;
    } catch {
      return false;
    }
  }

  async listTables(schema?: string): Promise<TableRef[]> {
    const result = await this.pool.query<TableRow>(
      `SELECT table_schema AS schema, table_name AS name, table_type AS type
       FROM information_schema.tables
       WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
         AND ($1::text IS NULL OR table_schema = $1)
       ORDER BY 1, 2`,
      [schema ?? null],
    );
    return result.rows;
  }

  async describeTable(table: string, schema?: string): Promise<TableDescription> {
    const columns = await this.pool.query<ColumnRow>(
      `SELECT c.column_name AS name,
              c.data_type AS type,
              (c.is_nullable = 'YES') AS nullable,
              c.column_default AS "default",
              EXISTS (
                SELECT 1
                FROM information_schema.table_constraints tc
                JOIN information_schema.key_column_usage kcu
                  ON tc.constraint_name = kcu.constraint_name
                 AND tc.table_schema = kcu.table_schema
                WHERE tc.constraint_type = 'PRIMARY KEY'
                  AND tc.table_name = c.table_name
                  AND tc.table_schema = c.table_schema
                  AND kcu.column_name = c.column_name
              ) AS "primaryKey"
       FROM information_schema.columns c
       WHERE c.table_name = $1
         AND c.table_schema = COALESCE($2, current_schema())
       ORDER BY c.ordinal_position`,
      [table, schema ?? null],
    );

    const indexes = await this.pool.query<IndexRow>(
      `SELECT i.relname AS name,
              ix.indisunique AS "unique",
              array_agg(a.attname ORDER BY k.ord) AS columns
       FROM pg_index ix
       JOIN pg_class t ON t.oid = ix.indrelid
       JOIN pg_class i ON i.oid = ix.indexrelid
       JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ord) ON TRUE
       JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
       WHERE t.relname = $1
         AND t.relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = COALESCE($2, current_schema()))
       GROUP BY i.relname, ix.indisunique
       ORDER BY i.relname`,
      [table, schema ?? null],
    );

    const foreignKeys = await this.pool.query<ForeignKeyRow>(
      `SELECT tc.constraint_name AS name,
              kcu.column_name AS column,
              ccu.table_name AS referenced_table,
              ccu.column_name AS referenced_column
       FROM information_schema.table_constraints tc
       JOIN information_schema.key_column_usage kcu
         ON tc.constraint_name = kcu.constraint_name
        AND tc.table_schema = kcu.table_schema
       JOIN information_schema.constraint_column_usage ccu
         ON ccu.constraint_name = tc.constraint_name
        AND ccu.table_schema = tc.table_schema
       WHERE tc.constraint_type = 'FOREIGN KEY'
         AND tc.table_name = $1
         AND tc.table_schema = COALESCE($2, current_schema())
       ORDER BY tc.constraint_name, kcu.ordinal_position`,
      [table, schema ?? null],
    );

    return {
      schema: schema ?? "public",
      table,
      columns: columns.rows.map((row) => ({
        name: row.name,
        type: row.type,
        nullable: row.nullable,
        default: row.default,
        primaryKey: row.primaryKey,
      })),
      indexes: groupIndexes(indexes.rows),
      foreignKeys: groupForeignKeys(foreignKeys.rows),
    };
  }

  async explain(sql: string, params: unknown[]): Promise<string> {
    const result = await this.pool.query(`EXPLAIN (FORMAT TEXT) ${sql}`, params);
    return result.rows
      .map((row) => String((row as Record<string, unknown>)["QUERY PLAN"] ?? ""))
      .join("\n");
  }

  async execute(sql: string, params: unknown[]): Promise<QueryResult> {
    const started = performance.now();
    const result = await this.pool.query(sql, params);
    return {
      columns: result.fields?.map((field) => field.name) ?? [],
      rows: result.rows as Record<string, unknown>[],
      rowCount: result.rowCount ?? result.rows.length,
      durationMs: Math.round(performance.now() - started),
    };
  }

  async close(): Promise<void> {
    await this.pool.end();
  }
}