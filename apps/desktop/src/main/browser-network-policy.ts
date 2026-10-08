import {
  BROWSER_BLOCKED_CIDRS,
  BROWSER_BLOCKED_HOSTNAMES,
  BROWSER_LOOPBACK_CIDRS,
  BROWSER_LOOPBACK_HOSTNAMES,
  BROWSER_PRIVATE_NETWORK_CIDRS,
} from "@magi/desktop-browser-contracts";

/**
 * 浏览器 guest 的网络边界求值器。
 *
 * 静态规则（元数据主机名、链路本地 / 云元数据地址、本机与私有网段）来自
 * `contracts/desktop-browser/network-policy.json`，daemon 的入口校验使用同一份；
 * 动态规则（Magi 自身 daemon 端口、是否放行本机与局域网）只属于 Main。
 * 导航入口、`will-navigate` 与 session 网络层拦截都走这一个求值器。
 */

export interface BrowserNetworkPolicyConfig {
  /** Magi 自身服务监听的端口：guest 不能访问本机上的这些来源。 */
  readonly selfPorts: ReadonlySet<number>;
  /** 是否放行本机与局域网地址。关闭后只允许公网目标。 */
  readonly allowPrivateNetwork: boolean;
}

export type BrowserUrlRejection =
  | "unsupported_protocol"
  | "credentials"
  | "blocked_target"
  | "self_origin"
  | "private_network";

export type BrowserUrlVerdict =
  | { readonly allowed: true }
  | { readonly allowed: false; readonly reason: BrowserUrlRejection };

type HostClass = "blocked" | "loopback" | "private" | "public";

interface Cidr {
  readonly bits: 32 | 128;
  readonly network: bigint;
  readonly prefix: number;
}

const NAVIGATION_PROTOCOLS = new Set(["http:", "https:", "about:"]);
const REQUEST_PROTOCOLS = new Set(["http:", "https:", "ws:", "wss:"]);
const IPV4_MAPPED_HIGH_BITS = 0xffffn;

function parseIpv4(text: string): bigint | null {
  const parts = text.split(".");
  if (parts.length !== 4) return null;
  let value = 0n;
  for (const part of parts) {
    if (!/^\d{1,3}$/u.test(part) || Number(part) > 255) return null;
    value = (value << 8n) | BigInt(part);
  }
  return value;
}

function parseIpv6(text: string): bigint | null {
  let address = text;
  // 末尾的点分 IPv4（::ffff:1.2.3.4）折算成两个 16 位组。
  const lastColon = address.lastIndexOf(":");
  if (address.includes(".")) {
    const v4 = parseIpv4(address.slice(lastColon + 1));
    if (v4 === null) return null;
    address = `${address.slice(0, lastColon + 1)}${(v4 >> 16n).toString(16)}:${(v4 & 0xffffn).toString(16)}`;
  }
  const halves = address.split("::");
  if (halves.length > 2) return null;
  const toGroups = (part: string): string[] | null =>
    part === "" ? [] : part.split(":");
  const head = toGroups(halves[0] ?? "");
  const tail = halves.length === 2 ? toGroups(halves[1] ?? "") : [];
  if (head === null || tail === null) return null;
  const missing = 8 - head.length - tail.length;
  if (halves.length === 2 ? missing < 1 : missing !== 0) return null;
  const groups = [...head, ...Array<string>(halves.length === 2 ? missing : 0).fill("0"), ...tail];
  let value = 0n;
  for (const group of groups) {
    if (!/^[0-9a-f]{1,4}$/iu.test(group)) return null;
    value = (value << 16n) | BigInt(`0x${group}`);
  }
  return value;
}

function parseCidr(text: string): Cidr {
  const [address, prefixText] = text.split("/");
  const prefix = Number(prefixText);
  if (address === undefined || !Number.isInteger(prefix)) {
    throw new Error(`browser_network_policy_cidr_invalid:${text}`);
  }
  const v4 = address.includes(":") ? null : parseIpv4(address);
  if (v4 !== null) return { bits: 32, network: v4, prefix };
  const v6 = parseIpv6(address);
  if (v6 === null) throw new Error(`browser_network_policy_cidr_invalid:${text}`);
  return { bits: 128, network: v6, prefix };
}

function cidrContains(cidr: Cidr, bits: 32 | 128, value: bigint): boolean {
  if (cidr.bits !== bits) return false;
  if (cidr.prefix === 0) return true;
  const shift = BigInt(bits - cidr.prefix);
  return value >> shift === cidr.network >> shift;
}

const BLOCKED_CIDRS = BROWSER_BLOCKED_CIDRS.map(parseCidr);
const LOOPBACK_CIDRS = BROWSER_LOOPBACK_CIDRS.map(parseCidr);
const PRIVATE_CIDRS = BROWSER_PRIVATE_NETWORK_CIDRS.map(parseCidr);

function hostnameMatches(host: string, names: readonly string[]): boolean {
  return names.some((name) => host === name || host.endsWith(`.${name}`));
}

function classifyAddress(bits: 32 | 128, value: bigint): HostClass {
  // IPv4 映射的 IPv6 地址（::ffff:a.b.c.d）与对应的 IPv4 地址是同一个目标。
  const candidates: Array<[32 | 128, bigint]> = [[bits, value]];
  if (bits === 128 && value >> 32n === IPV4_MAPPED_HIGH_BITS) {
    candidates.push([32, value & 0xffffffffn]);
  }
  const inAny = (cidrs: readonly Cidr[]) =>
    candidates.some(([candidateBits, candidate]) =>
      cidrs.some((cidr) => cidrContains(cidr, candidateBits, candidate)));
  if (inAny(BLOCKED_CIDRS)) return "blocked";
  if (inAny(LOOPBACK_CIDRS)) return "loopback";
  if (inAny(PRIVATE_CIDRS)) return "private";
  return "public";
}

function classifyHost(url: URL): HostClass {
  let host = url.hostname.toLowerCase();
  if (host.startsWith("[") && host.endsWith("]")) {
    const value = parseIpv6(host.slice(1, -1));
    return value === null ? "blocked" : classifyAddress(128, value);
  }
  host = host.replace(/\.+$/u, "");
  const v4 = parseIpv4(host);
  if (v4 !== null) return classifyAddress(32, v4);
  if (hostnameMatches(host, BROWSER_BLOCKED_HOSTNAMES)) return "blocked";
  if (hostnameMatches(host, BROWSER_LOOPBACK_HOSTNAMES)) return "loopback";
  return "public";
}

function effectivePort(url: URL): number {
  if (url.port) return Number(url.port);
  return url.protocol === "https:" || url.protocol === "wss:" ? 443 : 80;
}

/** 目标主机是否允许访问（不含协议与凭据规则）：子资源、重定向与 WebSocket 请求都用它。 */
export function evaluateBrowserRequestTarget(
  url: URL,
  config: BrowserNetworkPolicyConfig,
): BrowserUrlVerdict {
  if (url.protocol === "about:") return { allowed: true };
  if (!REQUEST_PROTOCOLS.has(url.protocol)) return { allowed: true };
  const hostClass = classifyHost(url);
  if (hostClass === "blocked") return { allowed: false, reason: "blocked_target" };
  if (hostClass === "loopback" && config.selfPorts.has(effectivePort(url))) {
    return { allowed: false, reason: "self_origin" };
  }
  if (!config.allowPrivateNetwork && hostClass !== "public") {
    return { allowed: false, reason: "private_network" };
  }
  return { allowed: true };
}

/** 导航入口的完整校验：协议白名单、不带凭据，再加上目标主机规则。 */
export function evaluateBrowserNavigation(
  url: URL,
  config: BrowserNetworkPolicyConfig,
): BrowserUrlVerdict {
  if (!NAVIGATION_PROTOCOLS.has(url.protocol)) {
    return { allowed: false, reason: "unsupported_protocol" };
  }
  if (url.username || url.password) return { allowed: false, reason: "credentials" };
  return evaluateBrowserRequestTarget(url, config);
}
