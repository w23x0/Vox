/** 页面跳转上下文：页面里的「去配置」之类链接用它切到别的页（页面状态住在 App）。 */

import { createContext, useContext } from "react";

export const NavigateContext = createContext<(page: string) => void>(() => undefined);

export function useNavigate(): (page: string) => void {
  return useContext(NavigateContext);
}
