import { createSignal, onCleanup, onMount } from "solid-js";

/** Mobile breakpoint used for intentional Terminal exclusion and compact controls. */
export const MOBILE_MAX_WIDTH = 900;

export function isMobileViewport(width = window.innerWidth): boolean {
  return width <= MOBILE_MAX_WIDTH;
}

/** Reactive viewport helper: true when the layout is treated as mobile. */
export function useIsMobile() {
  const [mobile, setMobile] = createSignal(
    typeof window !== "undefined" ? isMobileViewport() : false,
  );

  onMount(() => {
    const media = window.matchMedia(`(max-width: ${MOBILE_MAX_WIDTH}px)`);
    const update = () => setMobile(media.matches);
    update();
    media.addEventListener("change", update);
    onCleanup(() => media.removeEventListener("change", update));
  });

  return mobile;
}
