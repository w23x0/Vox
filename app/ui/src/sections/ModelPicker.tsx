/**
 * 首页「更换模型」弹层：先选服务商（只有配置了密钥的能选，没配的灰显并给「去配置」），
 * 再选模型（只列 `adapted=true` 的）。选定模型即提交并关闭。
 */

import { useEffect, useRef, useState } from "react";

import * as catalog from "../catalog";
import { useLang } from "../i18n/context";
import { useModelList } from "../lib/models";
import { useNavigate } from "../lib/navigate";
import { useStore } from "../store";
import type { ModelProvider } from "../types";

export function ModelPicker({
  provider,
  modelName,
  onPick,
  onClose,
}: {
  provider: ModelProvider;
  modelName: string;
  onPick: (provider: ModelProvider, model: string) => void;
  /** `byKeyboard`：是 Esc 关的（调用方据此把焦点还给「更换」按钮）。 */
  onClose: (byKeyboard: boolean) => void;
}) {
  const { t, uiLang } = useLang();
  const { snapshot } = useStore();
  const navigate = useNavigate();
  const [selected, setSelected] = useState<ModelProvider>(provider);
  const { list, loading } = useModelList(selected);
  const box = useRef<HTMLDivElement | null>(null);

  // 点弹层外面收起；挂在 document 上，「更换」按钮自己的点击已经 stopPropagation。
  useEffect(() => {
    const close = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) onClose(false);
    };
    // Esc 在捕获阶段拦下：不然 App 的全局 Esc（回侧栏）会抢先处理。
    const esc = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      e.preventDefault();
      onClose(true);
    };
    document.addEventListener("click", close);
    document.addEventListener("keydown", esc, true);
    return () => {
      document.removeEventListener("click", close);
      document.removeEventListener("keydown", esc, true);
    };
  }, [onClose]);

  const adapted = list.models.filter((m) => m.adapted);

  return (
    <div
      className="model-pop"
      ref={box}
      role="group"
      aria-label={t("pipeline.pickerAria")}
    >
      <div className="stat-label">{t("pipeline.pickProvider")}</div>
      <div className="pick-list">
        {catalog.providerIds().map((id) => {
          const configured = snapshot?.api_keys[id] ?? false;
          const label = catalog.providerLabel(id, uiLang);
          return (
            <div className="pick-row" key={id}>
              <button
                type="button"
                className={id === selected ? "pick-item selected" : "pick-item"}
                aria-pressed={id === selected}
                disabled={!configured}
                data-focus-item
                onClick={() => setSelected(id)}
              >
                {label}
              </button>
              {configured ? null : (
                <>
                  <span className="hint">{t("pipeline.notConfigured")}</span>
                  <button
                    type="button"
                    className="link-btn"
                    data-focus-item
                    onClick={() => navigate("models")}
                  >
                    {t("pipeline.goConfigure")}
                  </button>
                </>
              )}
            </div>
          );
        })}
      </div>

      <div className="stat-label" style={{ marginTop: 12 }}>
        {t("pipeline.pickModel")}
      </div>
      <div className="pick-list">
        {adapted.map((m) => (
          <button
            key={m.id}
            type="button"
            className={
              selected === provider && m.id === modelName ? "pick-item selected" : "pick-item"
            }
            aria-pressed={selected === provider && m.id === modelName}
            data-focus-item
            onClick={() => onPick(selected, m.id)}
          >
            <span>{m.label}</span>
            <span className="mono pick-id">{m.id}</span>
            {m.recommended ? <span className="badge badge-neutral">{t("models.recommended")}</span> : null}
          </button>
        ))}
        {adapted.length === 0 && !loading ? (
          <div className="hint">{t("pipeline.noModels")}</div>
        ) : null}
        {loading && adapted.length === 0 ? <div className="hint">{t("models.loading")}</div> : null}
      </div>
    </div>
  );
}
