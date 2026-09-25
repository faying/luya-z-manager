import { afterEach, describe, expect, it, vi } from "vitest";
import { isRemoteAccess, isRemoteHost } from "@/lib/api/remote";
import { setApiBase } from "@/lib/api/client";
import { stubWindow } from "./windowStub";

describe("isRemoteHost", () => {
  it.each([
    ["100.64.0.1", true],
    ["100.127.255.254", true],
    ["100.63.255.255", false],
    ["100.128.0.1", false],
    ["[fd7a:115c:a1e0::1]", true],
    ["fd7a:115c:a1e0:ab12:4843:cd96:6258:b240", true],
    ["FD7A:115C:A1E0::53", true],
    ["fd7a:115c:a1e1::1", false],
    ["fd7a:115c::1", false],
    ["::1", false],
    ["[fe80::1]", false],
    ["foo.ts.net", true],
    ["u60.tail1234.ts.net.", true],
    ["u60", true],
    ["U60", true],
    ["localhost", false],
    ["10.0.66.1", false],
    ["192.168.0.1", false],
    ["example.com", false],
    ["", false],
  ])("%s → %s", (host, want) => {
    expect(isRemoteHost(host)).toBe(want);
  });
});

describe("isRemoteAccess uses the API host (C2)", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("page on localhost, API over Tailscale → remote", () => {
    stubWindow("localhost", "http://localhost:3000");
    setApiBase("http://100.101.102.103:9090");
    expect(isRemoteAccess()).toBe(true);
  });

  it("page on a Tailscale name, API on the LAN address → not remote", () => {
    stubWindow("u60", "http://u60:9090");
    setApiBase("http://10.0.66.1:9090");
    expect(isRemoteAccess()).toBe(false);
  });

  it("same origin on a MagicDNS short name → remote", () => {
    stubWindow("u60", "http://u60:9090");
    expect(isRemoteAccess()).toBe(true);
  });

  it("IPv6 API base → remote", () => {
    stubWindow("localhost", "http://localhost:3000");
    setApiBase("http://[fd7a:115c:a1e0::5]:9090");
    expect(isRemoteAccess()).toBe(true);
  });
});
