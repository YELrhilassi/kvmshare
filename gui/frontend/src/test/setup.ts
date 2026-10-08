// Vitest setup.
//
//   - jest-dom matchers (toBeInTheDocument, etc.) for DOM assertions;
//   - explicit RTL cleanup after every test. Testing Library auto-registers
//     cleanup only when the framework's globals are on; these tests import
//     `describe`/`it` explicitly, so the DOM would otherwise accumulate
//     across tests and queries would match a previous render.
import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

afterEach(cleanup);
