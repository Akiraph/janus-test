interface SkeletonProps {
  compact?: boolean;
  class?: string;
}

export function Skeleton(props: SkeletonProps) {
  return (
    <div
      class="ui-skeleton"
      classList={{
        "ui-skeleton--compact": !!props.compact,
        [props.class ?? ""]: !!props.class,
      }}
      aria-hidden="true"
    />
  );
}
