import { describe, expect, it } from "vitest";
import { connectFamilies, connectKind, dataSwitchOn } from "@/lib/connectState";

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
  it("reads the boot-default enable 0 with the data up as on", () => {
    expect(dataSwitchOn(0, "ipv4_ipv6_connected")).toBe(true);
    expect(dataSwitchOn("0", "connecting")).toBe(true);
    expect(dataSwitchOn(0, "disconnected")).toBe(false);
    expect(dataSwitchOn(0, "ipv4_ipv6_disconnecting")).toBe(false);
    expect(dataSwitchOn(0, "")).toBeNull();
    expect(dataSwitchOn(1, "disconnected")).toBe(true);
    expect(dataSwitchOn(undefined, "ipv4_connected")).toBeNull();
  });
});
