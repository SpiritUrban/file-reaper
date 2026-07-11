/** HashRouter без server rewrite; AppLayout є постійним host усіх екранів. */

import { createHashRouter } from "react-router-dom";

import { AppLayout } from "./layout/AppLayout";

export const router = createHashRouter([
  {
    path: "*",
    element: <AppLayout />,
  },
]);