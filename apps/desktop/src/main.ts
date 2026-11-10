// Wyrd desktop frontend entry point.
//
// Svelte 5 shell; the backend is reached through Tauri IPC only
// (`src/lib/ipc/client.ts`) — never the owner socket, never wire
// bytes, never the drive directory. Frontend state is a
// presentation cache (`src/lib/state/presentation.svelte.ts`),
// never a second drive model.

import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";

const app = document.getElementById("app");

if (app) {
  mount(App, { target: app });
}
