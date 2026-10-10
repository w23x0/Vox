/**
 * 假后端的网络体检数据（`test_network` / `detect_takeover`）。
 *
 * 真后端的这两条命令在 `crates/vox-host/src/netcheck.rs`（共享实现，桌面与无屏都能调），
 * 这里只造**同样形状**的假数据，让模型页「网络」卡在没有 Rust 侧时也整套点得通。
 *
 * URL 开关：
 * - `?mockTun=1`——"代理软件接管了网络"那一档：假 IP 命中 + 虚拟网卡，并且阿里云
 *   国内站被绕到海外（延迟难看 + 换地区建议）。对应真机上的 Clash TUN / Meta 假 IP 模式。
 * - `?mockNetFail=1`——所有接入点都连不上（连接被拒绝），走错误行的渲染。
 */

import type {
  EndpointResult,
  ModelProvider,
  NetworkFailureKind,
  NetworkReport,
  NetworkSetting,
  TakeoverReport,
} from "../types";

/** 与服务商 id → 候选接入点同一张表（`vox_host::netcheck::endpoints_of`）。 */
const ENDPOINTS: Record<ModelProvider, { id: string; host: string }[]> = {
  aliyun: [
    { id: "cn", host: "dashscope.aliyuncs.com" },
    { id: "intl", host: "dashscope-intl.aliyuncs.com" },
    { id: "hk", host: "cn-hongkong.dashscope.aliyuncs.com" },
  ],
  gpt: [{ id: "default", host: "api.openai.com" }],
  gemini: [{ id: "default", host: "generativelanguage.googleapis.com" }],
};

/**
 * 把 `scheme://user:pass@host:port` 拆成（去掉 userinfo 的地址, 是否带过 userinfo）。
 * 镜像 Rust `vox_core::settings::split_proxy_credentials`——假后端脱敏与真后端同一条规则。
 */
export function splitProxyCredentials(url: string): [string, boolean] {
  const at = url.lastIndexOf("@");
  if (at < 0) return [url, false];
  const schemeEnd = url.indexOf("://");
  const authorityStart = schemeEnd < 0 ? 0 : schemeEnd + 3;
  // `@` 出现在 authority 之前（路径里的查询参数）不算账号。
  if (at < authorityStart) return [url, false];
  return [`${url.slice(0, authorityStart)}${url.slice(at + 1)}`, true];
}

/** 演示用的各段耗时（毫秒）：直连时国内站最近，海外绕一圈就难看。 */
const TIMINGS: Record<string, { dns: number; connect: number; tls: number; http: number }> = {
  "dashscope.aliyuncs.com": { dns: 11, connect: 26, tls: 88, http: 118 },
  "cn-hongkong.dashscope.aliyuncs.com": { dns: 34, connect: 92, tls: 148, http: 176 },
  "dashscope-intl.aliyuncs.com": { dns: 42, connect: 186, tls: 252, http: 296 },
  "api.openai.com": { dns: 9, connect: 148, tls: 214, http: 244 },
  "generativelanguage.googleapis.com": { dns: 12, connect: 136, tls: 198, http: 226 },
};

/** `?mockTun=1` 时阿里云国内站的延迟：流量被代理软件绕到海外后的样子。 */
const TUN_DETOUR: Record<string, { dns: number; connect: number; tls: number; http: number }> = {
  "dashscope.aliyuncs.com": { dns: 7, connect: 212, tls: 318, http: 356 },
};

const end = (id: string, host: string, current: boolean): EndpointResult => ({
  id,
  host,
  current,
  dns_ms: null,
  connect_ms: null,
  tunnel_ms: null,
  tls_ms: null,
  http_ms: null,
  http_status: null,
  total_ms: null,
  error: null,
});

function failed(result: EndpointResult, kind: NetworkFailureKind, message: string): EndpointResult {
  return { ...result, error: { kind, message } };
}

/** 假后端"探测到"的系统代理（真后端读环境变量 / 注册表，可能什么都没有）。 */
const MOCK_SYSTEM_PROXY = "http://127.0.0.1:7890";

/** 按当前网络设置挑一个"实际使用的代理"显示串（真后端 `NetworkReport.via`）。 */
function viaOf(network: NetworkSetting | undefined): string | null {
  const mode = network?.mode ?? "direct";
  if (mode === "system") return MOCK_SYSTEM_PROXY;
  if (mode === "custom") {
    const proxy = (network?.proxy ?? "").trim();
    // 与 Rust `ProxySpec::display()` 同一条：账号密码不显示。
    return proxy ? splitProxyCredentials(proxy)[0] : null;
  }
  return null;
}

export interface MockNetFlags {
  /** `?mockTun=1` */
  tun: boolean;
  /** `?mockNetFail=1` */
  fail: boolean;
}

export function mockNetworkFlags(search = window.location.search): MockNetFlags {
  const params = new URLSearchParams(search);
  return { tun: params.get("mockTun") === "1", fail: params.get("mockNetFail") === "1" };
}

export function mockNetworkReport(
  provider: ModelProvider,
  network: NetworkSetting | undefined,
  flags: MockNetFlags,
): NetworkReport {
  const endpoints = ENDPOINTS[provider];
  const results = endpoints.map(({ id, host }) => {
    const result = end(id, host, id === "cn" || id === "default");
    if (flags.fail) {
      return failed(result, "connect", "Connection refused (127.0.0.1:7890)");
    }
    const timed = TIMINGS[host] ?? { dns: 10, connect: 60, tls: 120, http: 150 };
    const detoured = flags.tun ? TUN_DETOUR[host] : undefined;
    const t = detoured ?? timed;
    const total = t.dns + t.connect + t.tls + t.http;
    return {
      ...result,
      dns_ms: t.dns,
      connect_ms: t.connect,
      tls_ms: t.tls,
      http_ms: t.http,
      http_status: 401,
      total_ms: total,
    };
  });

  // 换地区建议：和 Rust 同一条门槛（另一地区快 1.5 倍以上、且至少快 80ms）。
  const current = results.find((r) => r.current);
  const suggestion =
    provider === "aliyun" && current && current.total_ms !== null
      ? (() => {
          const best = results
            .filter((r) => !r.current && r.total_ms !== null)
            .sort((a, b) => (a.total_ms ?? 0) - (b.total_ms ?? 0))[0];
          if (!best || best.total_ms === null || best.total_ms === undefined) return null;
          const much = current.total_ms >= best.total_ms * 1.5;
          const gain = current.total_ms - best.total_ms >= 80;
          return much && gain
            ? {
                id: best.id,
                host: best.host,
                current_ms: current.total_ms,
                nearest_ms: best.total_ms,
              }
            : null;
        })()
      : null;

  return {
    provider,
    mode: network?.mode ?? "direct",
    via: viaOf(network),
    results,
    suggestion,
  };
}

export function mockTakeoverReport(provider: ModelProvider, flags: MockNetFlags): TakeoverReport {
  if (!flags.tun) {
    return {
      detected: false,
      fake_ip: false,
      fake_ip_hosts: [],
      interfaces: [],
      interfaces_scanned: true,
    };
  }
  // 假 IP 模式下服务商域名解析进 198.18.0.0/15；另外扫到一个代理软件的虚拟网卡。
  const hosts = ENDPOINTS[provider].map(({ host }) => host);
  return {
    detected: true,
    fake_ip: true,
    fake_ip_hosts: hosts,
    interfaces: ["utun3"],
    interfaces_scanned: true,
  };
}
