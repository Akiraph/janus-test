interface JanusLogoProps {
  size?: number;
  class?: string;
}

export function JanusLogo(props: JanusLogoProps) {
  const size = () => props.size ?? 32;
  return (
    <svg
      width={size()}
      height={size()}
      viewBox="0 0 32 32"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      class={props.class}
      aria-hidden="true"
    >
      <path
        d="M20 6C20 5.44772 20.4477 5 21 5H25C25.5523 5 26 5.44772 26 6V18C26 23.5228 21.5228 28 16 28C10.4772 28 6 23.5228 6 18V17C6 16.4477 6.44772 16 7 16H11C11.5523 16 12 16.4477 12 17V18C12 20.2091 13.7909 22 16 22C18.2091 22 20 20.2091 20 18V6Z"
        fill="var(--accent-strong)"
      />
    </svg>
  );
}
