import { useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import styles from './DataTable.module.css';
import { EmptyState } from '../EmptyState/EmptyState';
import { Spinner } from '../Spinner/Spinner';
import { Button } from '../Button/Button';
import { Icon } from '../Icon/Icon';
import { ChevronLeft, ChevronRight, ArrowUpDown, ArrowUp, ArrowDown } from 'lucide-react';

export type Row = Record<string, unknown>;

export interface DataTableColumn {
  key: string;
  title: string;
  width?: string | number;
  sortable?: boolean;
  render?: (row: Row) => ReactNode;
}

export interface DataTableProps {
  columns: DataTableColumn[];
  rows: Row[];
  loading?: boolean;
  pageSize?: number;
  emptyTitle?: string;
  emptyDescription?: string;
  className?: string;
}

type SortState = { key: string; direction: 'asc' | 'desc' } | null;

function sortRows(rows: Row[], sort: SortState): Row[] {
  if (!sort) return rows;
  const { key, direction } = sort;
  const multiplier = direction === 'asc' ? 1 : -1;
  return [...rows].sort((a, b) => {
    const av = a[key];
    const bv = b[key];
    if (typeof av === 'number' && typeof bv === 'number') return (av - bv) * multiplier;
    const as = String(av ?? '');
    const bs = String(bv ?? '');
    return as.localeCompare(bs) * multiplier;
  });
}

export function DataTable({
  columns,
  rows,
  loading = false,
  pageSize = 20,
  emptyTitle = 'No data',
  emptyDescription,
  className = '',
}: DataTableProps) {
  const [sort, setSort] = useState<SortState>(null);
  const [page, setPage] = useState(1);

  const sortedRows = useMemo(() => sortRows(rows, sort), [rows, sort]);
  const totalPages = Math.max(1, Math.ceil(sortedRows.length / pageSize));
  const safePage = Math.min(page, totalPages);
  const pageRows = useMemo(() => {
    const start = (safePage - 1) * pageSize;
    return sortedRows.slice(start, start + pageSize);
  }, [sortedRows, safePage, pageSize]);

  const handleSort = (key: string) => {
    setSort((prev) => {
      if (!prev || prev.key !== key) return { key, direction: 'asc' };
      if (prev.direction === 'asc') return { key, direction: 'desc' };
      return null;
    });
    setPage(1);
  };

  const start = rows.length === 0 ? 0 : (safePage - 1) * pageSize + 1;
  const end = Math.min(safePage * pageSize, rows.length);

  return (
    <div className={[styles.wrapper, className].filter(Boolean).join(' ')}>
      <div className={styles.scroll}>
        <table className={styles.table}>
          <thead>
            <tr>
              {columns.map((col, index) => (
                <th
                  key={col.key}
                  className={[styles.th, index === 0 ? styles.stickyStart : ''].filter(Boolean).join(' ')}
                  style={{ width: col.width, minWidth: col.width }}
                  onClick={() => col.sortable && handleSort(col.key)}
                  aria-sort={
                    sort?.key === col.key ? (sort.direction === 'asc' ? 'ascending' : 'descending') : 'none'
                  }
                >
                  <span className={styles.headerCell}>
                    {col.title}
                    {col.sortable && (
                      <span className={styles.sortIcon}>
                        {sort?.key === col.key ? (
                          sort.direction === 'asc' ? (
                            <Icon icon={ArrowUp} size={12} />
                          ) : (
                            <Icon icon={ArrowDown} size={12} />
                          )
                        ) : (
                          <Icon icon={ArrowUpDown} size={12} />
                        )}
                      </span>
                    )}
                  </span>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {pageRows.map((row, rowIndex) => (
              <tr key={String(row.id ?? row.openalex_id ?? rowIndex)}>
                {columns.map((col, colIndex) => (
                  <td
                    key={col.key}
                    className={[styles.td, colIndex === 0 ? styles.stickyStart : ''].filter(Boolean).join(' ')}
                    style={{ width: col.width, minWidth: col.width }}
                  >
                    {col.render ? col.render(row) : String(row[col.key] ?? '—')}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
        {loading && (
          <div className={styles.loading}>
            <Spinner size="medium" />
          </div>
        )}
      </div>
      {rows.length === 0 && !loading && (
        <EmptyState title={emptyTitle} description={emptyDescription} />
      )}
      {rows.length > 0 && (
        <div className={styles.pagination}>
          <span className={styles.range}>
            {start}–{end} / {rows.length}
          </span>
          <div className={styles.pageActions}>
            <Button
              variant="secondary"
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              disabled={safePage === 1}
              aria-label="Previous page"
            >
              <Icon icon={ChevronLeft} size={16} />
            </Button>
            <span className={styles.pageInfo}>
              {safePage} / {totalPages}
            </span>
            <Button
              variant="secondary"
              onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
              disabled={safePage === totalPages}
              aria-label="Next page"
            >
              <Icon icon={ChevronRight} size={16} />
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
