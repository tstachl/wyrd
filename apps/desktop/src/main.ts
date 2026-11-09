// Wyrd desktop frontend shell (scaffold): no views yet, only the
// discipline the views will follow.
//
// - This directory holds no Rust code and takes no Cargo membership:
//   it is invisible to the dependency-DAG checker by construction.
// - The frontend talks to the backend through Tauri IPC only — never
//   to the owner socket, never to the wire bytes, never to the drive
//   directory. Every interface response keeps its typed semantics
//   end to end (Unavailable and Unconfirmed stay distinct; refusals
//   keep their codes; an unconfirmed mutation never becomes success
//   through mapping).
// - Frontend state is a presentation cache, never a second drive
//   model: selected view, open dialogs, form input,
//   pending-request indicators, last observed response. Membership,
//   lifecycle, materialization, status, and mutation outcomes live in
//   the owner; the display follows what the owner last reported.
// - The unavailable state is a first-class view, not an error toast:
//   owner down shows it, the shell never opens the store, and owner
//   back resumes without restart.
//
// Until the Tauri command layer lands (first view), this entry point
// renders the scaffold placeholder so `bun run dev` / `bun run
// build` (wired from the backend crate's `tauri.conf.json`) have a
// real artifact to serve.

const app = document.getElementById("app");

if (app) {
  const heading = document.createElement("h1");
  heading.textContent = "Wyrd";
  const status = document.createElement("p");
  status.textContent = "Desktop shell scaffold: the owner connection is not wired yet.";
  app.append(heading, status);
}
