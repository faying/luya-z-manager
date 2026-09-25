import { describe, expect, it } from "vitest";
import { connectFamilies, connectKind } from "@/lib/connectState";

describe("connect_status", () => {
  it("treats every *_connected word as connected", () => {
    expect(connectKind("ipv4_ipv6_connected")).toBe("connected");
    expect(connectKind("ipv4_connected")).toBe("connected");
    expect(connectKind("connected")).toBe("connected");
  });
  it("keeps connecting and disconnected apart", () => {
    expect(connectKind("ipv4_connecting")).toBe("connecting");
    expect(connectKind("disconnected")).toBe("disconnected");
    expect(connectKind("ipv4_ipv6_disconnecting")).toBe("disconnected");
    expect(connectKind("")).toBe("unknown");
  });
  it("names the families", () => {
    expect(connectFamilies("ipv4_ipv6_connected")).toBe("IPv4 + IPv6");
    expect(connectFamilies("ipv6_connected")).toBe("IPv6");
    expect(connectFamilies("connected")).toBeNull();
  });
});
