import { StrictMode, useEffect } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import { LanguageProvider } from "./i18n/context";
import { detectUiLang } from "./i18n/types";
import { StoreProvider, useStore } from "./store";
import { ToastProvider } from "./ui/toast";

/* 顺序要紧：fonts 先注册 @font-face，tokens 落变量，shell/components 都依赖变量。 */
import "./styles/fonts.css";
import "./styles/tokens.css";
import "./styles/shell.css";
import "./styles/components.css";
import "./styles/subtitle.css";

const host = document.getElementById("root");
if (!host) throw new Error("找不到 #root，index.html 被改坏了。");

/** 首次启动按系统语言选界面语言的标记；有了它就再也不替用户改语言。 */
const LANG_INIT_KEY = "vox.ui-lang.initialized";

/** 从 store 取持久化的界面语言，注入 LanguageProvider（语言驱动源）。 */
function LangBridge({ children }: { children: React.ReactNode }) {
  const { settings, snapshot, patch } = useStore();

  // 首次启动：后端设置到手之后，按系统语言选一次（系统语言不支持就 English）。
  useEffect(() => {
    if (!snapshot) return;
    try {
      if (localStorage.getItem(LANG_INIT_KEY)) return;
      localStorage.setItem(LANG_INIT_KEY, "1");
    } catch {
      return; // 存不下标记就不动语言，免得每次启动都改一遍
    }
    const detected = detectUiLang(navigator.language);
    if (detected !== snapshot.settings.ui_language) patch({ ui_language: detected });
  }, [snapshot, patch]);

  return <LanguageProvider uiLang={settings.ui_language}>{children}</LanguageProvider>;
}

createRoot(host).render(
  <StrictMode>
    <StoreProvider>
      <LangBridge>
        <ToastProvider>
          <App />
        </ToastProvider>
      </LangBridge>
    </StoreProvider>
  </StrictMode>,
);
