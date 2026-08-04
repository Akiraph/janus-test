import { createMemo, createSignal, For } from "solid-js";
import "./tabs.css";

export interface TabItem {
  value: string;
  label: string;
  disabled?: boolean;
}

interface TabsProps {
  /** 当前选中值(受控) */
  value: string;
  /** 切换回调;禁用项不会触发 */
  onChange: (value: string) => void;
  /** tab 列表 */
  tabs: TabItem[];
  /** a11y:tablist 的可访问标签 */
  "aria-label"?: string;
  class?: string;
}

/**
 * 受控单选 Tabs — WAI-ARIA tab 模式 + roving tabindex 键盘导航。
 * 分段式(seg)容器:仅切换器,内容不由本组件承载。
 */
export function Tabs(props: TabsProps) {
  const [focused, setFocused] = createSignal(props.value);

  const enabled = createMemo(() => props.tabs.filter((t) => !t.disabled));

  function focusAt(value: string) {
    setFocused(value);
    document.getElementById(tabId(value))?.focus();
  }

  function move(direction: 1 | -1) {
    const list = enabled();
    if (list.length === 0) return;
    let idx = list.findIndex((t) => t.value === focused());
    if (idx === -1) idx = direction === 1 ? -1 : list.length;
    const next = list[(idx + direction + list.length) % list.length];
    if (next) focusAt(next.value);
  }

  function focusEdge(which: "first" | "last") {
    const list = enabled();
    if (list.length === 0) return;
    const next = which === "first" ? list[0] : list[list.length - 1];
    if (next) focusAt(next.value);
  }

  function onKeyDown(e: KeyboardEvent) {
    switch (e.key) {
      case "ArrowRight":
      case "ArrowDown":
        e.preventDefault();
        move(1);
        break;
      case "ArrowLeft":
      case "ArrowUp":
        e.preventDefault();
        move(-1);
        break;
      case "Home":
        e.preventDefault();
        focusEdge("first");
        break;
      case "End":
        e.preventDefault();
        focusEdge("last");
        break;
    }
  }

  const tabId = (value: string) => `ui-tab-${value}`;

  return (
    <div
      class="ui-tabs"
      classList={{ [props.class ?? ""]: !!props.class }}
      role="tablist"
      aria-label={props["aria-label"]}
      onKeyDown={onKeyDown}
    >
      <For each={props.tabs}>
        {(tab) => {
          const selected = () => props.value === tab.value;
          return (
            <button
              type="button"
              role="tab"
              id={tabId(tab.value)}
              class="ui-tabs__tab"
              classList={{
                "ui-tabs__tab--active": selected(),
                "ui-tabs__tab--disabled": !!tab.disabled,
              }}
              aria-selected={selected()}
              aria-disabled={tab.disabled ?? false}
              tabIndex={focused() === tab.value ? 0 : -1}
              disabled={tab.disabled ?? false}
              onClick={() => {
                if (tab.disabled) return;
                setFocused(tab.value);
                props.onChange(tab.value);
              }}
              onFocus={() => setFocused(tab.value)}
            >
              <span class="ui-tabs__label">{tab.label}</span>
            </button>
          );
        }}
      </For>
    </div>
  );
}
