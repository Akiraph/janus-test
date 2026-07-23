import { AlertCircle, RefreshCw } from "lucide-solid";

export function QuerySkeleton() {
  return (
    <div class="query-skeleton" role="status" aria-label="Loading system status">
      <span />
      <span />
      <span />
    </div>
  );
}

interface QueryErrorProps {
  retry: () => void;
}

export function QueryError(props: QueryErrorProps) {
  return (
    <div class="query-error" role="alert">
      <AlertCircle size={18} />
      <span>System status unavailable</span>
      <button type="button" class="text-button" onClick={props.retry}>
        <RefreshCw size={14} />
        Retry
      </button>
    </div>
  );
}
