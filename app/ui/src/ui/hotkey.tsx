/**
 * 热键的显示与编辑，用规格里现成的两件拼出来：
 *   - 修饰键 Ctrl / Alt / Shift → chip（等宽小标签，active 是绿描边+绿字）
 *   - 主键 → 自绘下拉
 * 没有另造键帽样式。
 */

import { useT } from "../i18n/context";
import type { Hotkey } from "../types";
import { Dropdown, type Option } from "./controls";

const MODIFIERS = [
  { key: "ctrl", label: "Ctrl" },
  { key: "alt", label: "Alt" },
  { key: "shift", label: "Shift" },
] as const;

const LETTERS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ".split("");
const DIGITS = "0123456789".split("");

/** 主键下拉的选项：字母、数字、F1–F12、空格、鼠标侧键（后三者的名字随界面语言）。 */
function keyOptions(t: ReturnType<typeof useT>): Option[] {
  return [
    ...LETTERS.map((key) => ({ value: key, label: key })),
    ...DIGITS.map((key) => ({ value: key, label: key })),
    ...Array.from({ length: 12 }, (_, index) => ({ value: `F${index + 1}`, label: `F${index + 1}` })),
    { value: "Space", label: t("settings.keySpace") },
    { value: "XButton1", label: t("settings.keyMouse1") },
    { value: "XButton2", label: t("settings.keyMouse2") },
  ];
}

/** 可编辑：三个修饰键 chip + 主键下拉。撞键时描红提示在调用方给。 */
export function HotkeyEditor({
  id,
  hotkey,
  onChange,
  disabled,
  label,
}: {
  id?: string;
  hotkey: Hotkey;
  onChange: (next: Hotkey) => void;
  disabled?: boolean;
  label?: string;
}) {
  const t = useT();
  const dropdownLabel = label
    ? t("settings.primaryKeyFor", { label })
    : t("settings.primaryKey");
  return (
    <span className="row" style={{ gap: 6 }}>
      {MODIFIERS.map((m) => {
        const on = hotkey[m.key];
        return (
          <button
            key={m.key}
            type="button"
            className={on ? "chip selected" : "chip"}
            aria-pressed={on}
            disabled={disabled}
            data-focus-item
            onClick={() => onChange({ ...hotkey, [m.key]: !on })}
          >
            {m.label}
          </button>
        );
      })}
      <span style={{ minWidth: 112 }}>
        <Dropdown
          id={id}
          label={dropdownLabel}
          value={hotkey.key}
          options={keyOptions(t)}
          disabled={disabled}
          onChange={(key) => onChange({ ...hotkey, key })}
        />
      </span>
    </span>
  );
}
