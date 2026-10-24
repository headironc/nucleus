import { index, route, type RouteConfig } from "@react-router/dev/routes";

export default [
  index("routes/home.tsx"),
  route("authentication/codes", "routes/codes.tsx"),
  route("authentication/login", "routes/login.tsx"),
  route("authentication/register", "routes/register.tsx"),
  route("account", "routes/account.tsx"),
  route("account/affiliation", "routes/affiliation.tsx"),
  route("account/cancellation", "routes/cancellation.tsx"),
  route("workforce/termination", "routes/workforce-termination.tsx"),
  route("workforce", "routes/workforce.tsx"),
  route("sessions/logout", "routes/logout.tsx"),
  route("sessions/logout-all", "routes/logout-all.tsx"),
  route("*", "routes/missing.tsx"),
] satisfies RouteConfig;
