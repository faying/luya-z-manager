// Every mock route, in one list. Imported by server.ts and record.ts
// (record.ts calls the handlers directly to diff against real responses).

import type { Route } from "./lib.ts";
import { routes as publicRoutes } from "./fixtures/public.ts";
import { routes as networkRoutes } from "./fixtures/network.ts";
import { routes as deviceRoutes } from "./fixtures/device.ts";
import { routes as systemRoutes } from "./fixtures/system.ts";
import { routes as toolsRoutes } from "./fixtures/tools.ts";
import { routes as wifiRoutes } from "./fixtures/wifi.ts";
import { routes as modemRoutes } from "./fixtures/modem.ts";
import { routes as simRoutes } from "./fixtures/sim.ts";
import { routes as telephonyRoutes } from "./fixtures/telephony.ts";
import { routes as routerRoutes } from "./fixtures/router.ts";
import { routes as servicesRoutes } from "./fixtures/services.ts";
import { routes as scenarioRoutes } from "./fixtures/scenario.ts";
import { routes as smsRoutes } from "./fixtures/sms.ts";
import { routes as esimRoutes } from "./fixtures/esim.ts";

export const ALL_ROUTES: Route[] = [
  ...publicRoutes,
  ...networkRoutes,
  ...deviceRoutes,
  ...systemRoutes,
  ...toolsRoutes,
  ...wifiRoutes,
  ...modemRoutes,
  ...simRoutes,
  ...telephonyRoutes,
  ...routerRoutes,
  ...servicesRoutes,
  ...scenarioRoutes,
  ...smsRoutes,
  ...esimRoutes,
];
