import { ConnectionPool, type IResult, type config as SqlConfig } from "mssql";
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
  nullable: number;
  default: string | null;
  primaryKey: number;
}

interface IndexRow {
  name: string;
  unique: boolean;
  column: string;
  ordinal: number;
}

interface ForeignKeyRow {
  name: string;
  column: string;
  referenced_table: string;
  referenced_column: string;
  ordinal: number;
}

interface TableRow {
  schema: string;
  name: string;
  type: string;
}

export class MssqlDriver implements Driver {
  readonly driver: DriverName = "mssql";
  readonly role: PoolRole;
  private readonly pool: ConnectionPool;

  constructor(dsn: string, role: PoolRole, statementTimeoutMs?: number) {
    this.role = role;
    const config: SqlConfig = ConnectionPool.parseConnectionString(dsn);
    config.pool = { ...config.pool, max: 5, min: 0 };
    config.connectionTimeout = 5_000;
    if (statementTimeoutMs) config.requestTimeout = statementTimeoutMs;
    this.pool = new ConnectionPool(config);
  }

  async init(): Promise<void> {
    await this.pool.connect();
  }

  async ping(): Promise<boolean> {
    try {
      await this.pool.request().query("SELECT 1");
      return true;
    } catch {
      return false;
    }
  }

  /** SQL Server uses @p1, @p2... placeholders, matching the params array. */
  private run(sql: string, params: unknown[]): Promise<IResult<Record<string, unknown>>> {
    const request = this.pool.request();
    params.forEach((value, index) => request.input(`p${index + 1}`, value as never));
    return request.query<Record<string, unknown>>(sql);
  }

  async listTables(schema?: string): Promise<TableRef[]> {
    const result = await this.run(
      `SELECT TABLE_SCHEMA AS [schema], TABLE_NAME AS [name], TABLE_TYPE AS [type]
       FROM INFORMATION_SCHEMA.TABLES
       WHERE TABLE_SCHEMA NOT IN ('sys', 'INFORMATION_SCHEMA')
         AND (@p1 IS NULL OR TABLE_SCHEMA = @p1)
       ORDER BY 1, 2`,
      [schema ?? null],
    );
    return (result.recordset ?? []) as unknown as TableRow[];
  }

  async describeTable(table: string, schema?: string): Promise<TableDescription> {
    const columns = await this.run(
      `SELECT c.COLUMN_NAME AS [name],
              c.DATA_TYPE AS [type],
              CASE WHEN c.IS_NULLABLE = 'YES' THEN 1 ELSE 0 END AS [nullable],
              c.COLUMN_DEFAULT AS [default],
              CASE WHEN pk.COLUMN_NAME IS NULL THEN 0 ELSE 1 END AS [primaryKey]
       FROM INFORMATION_SCHEMA.COLUMNS c
       LEFT JOIN (
         SELECT kcu.COLUMN_NAME
         FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc
         JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu
           ON tc.CONSTRAINT_NAME = kcu.CONSTRAINT_NAME
          AND tc.TABLE_SCHEMA = kcu.TABLE_SCHEMA
         WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY'
           AND tc.TABLE_NAME = @p1
           AND tc.TABLE_SCHEMA = COALESCE(@p2, SCHEMA_NAME())
       ) pk ON pk.COLUMN_NAME = c.COLUMN_NAME
       WHERE c.TABLE_NAME = @p1
         AND c.TABLE_SCHEMA = COALESCE(@p2, SCHEMA_NAME())
       ORDER BY c.ORDINAL_POSITION`,
      [table, schema ?? null],
    );

    const indexes = await this.run(
      `SELECT i.name AS [name],
              i.is_unique AS [unique],
              col.name AS [column],
              ic.key_ordinal AS [ordinal]
       FROM sys.indexes i
       JOIN sys.index_columns ic ON i.object_id = ic.object_id AND i.index_id = ic.index_id
       JOIN sys.columns col ON ic.object_id = col.object_id AND ic.column_id = col.column_id
       JOIN sys.tables t ON i.object_id = t.object_id
       JOIN sys.schemas s ON t.schema_id = s.schema_id
       WHERE t.name = @p1 AND s.name = COALESCE(@p2, SCHEMA_NAME())
       ORDER BY i.name, ic.key_ordinal`,
      [table, schema ?? null],
    );

    const foreignKeys = await this.run(
      `SELECT fk.name AS [name],
              pc.name AS [column],
              rt.name AS referenced_table,
              rc.name AS referenced_column,
              fkc.constraint_column_id AS [ordinal]
       FROM sys.foreign_keys fk
       JOIN sys.foreign_key_columns fkc ON fk.object_id = fkc.constraint_object_id
       JOIN sys.tables t ON fk.parent_object_id = t.object_id
       JOIN sys.schemas s ON t.schema_id = s.schema_id
       JOIN sys.columns pc ON fkc.parent_object_id = pc.object_id AND fkc.parent_column_id = pc.column_id
       JOIN sys.tables rt ON fkc.referenced_object_id = rt.object_id
       JOIN sys.columns rc ON fkc.referenced_object_id = rc.object_id AND fkc.referenced_column_id = rc.column_id
       WHERE t.name = @p1 AND s.name = COALESCE(@p2, SCHEMA_NAME())
       ORDER BY fk.name, fkc.constraint_column_id`,
      [table, schema ?? null],
    );

    const indexRows = (indexes.recordset ?? []) as unknown as IndexRow[];
    const indexList: TableIndex[] = [];
    for (const row of indexRows) {
      let index = indexList.find((item) => item.name === row.name);
      if (!index) {
        index = { name: row.name, unique: Boolean(row.unique), columns: [] };
        indexList.push(index);
      }
      index.columns.push(row.column);
    }

    const fkRows = (foreignKeys.recordset ?? []) as unknown as ForeignKeyRow[];
    const fkMap = new Map<string, TableForeignKey>();
    for (const row of fkRows) {
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
      schema: schema ?? "dbo",
      table,
      columns: ((columns.recordset ?? []) as unknown as ColumnRow[]).map(
        (row): TableColumn => ({
          name: row.name,
          type: row.type,
          nullable: Boolean(row.nullable),
          default: row.default,
          primaryKey: Boolean(row.primaryKey),
        }),
      ),
      indexes: indexList,
      foreignKeys: [...fkMap.values()],
    };
  }

  async explain(): Promise<string> {
    throw new Error("explain_query is not supported for the mssql driver");
  }

  async execute(sql: string, params: unknown[]): Promise<QueryResult> {
    const started = performance.now();
    const result = await this.run(sql, params);
    const rows = (result.recordset ?? []) as Record<string, unknown>[];
    const columns = result.recordset?.columns ? Object.keys(result.recordset.columns) : [];
    return {
      columns,
      rows,
      rowCount: rows.length > 0 ? rows.length : (result.rowsAffected?.[0] ?? 0),
      durationMs: Math.round(performance.now() - started),
    };
  }

  async close(): Promise<void> {
    await this.pool.close();
  }
}