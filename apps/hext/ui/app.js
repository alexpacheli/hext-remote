// The app's pages. What comes from the other computer (names) only ever
// goes in through textContent: it is text, never markup.
"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);

/** Aceitar waits this long: a window that pops up while the user is typing
 * or clicking must not be answered by accident. */
const ACCEPT_DELAY_MS = 1000;

const pages = {
  home: {
    start() {
      $("connect").addEventListener("submit", async (event) => {
        event.preventDefault();
        try {
          await invoke("connect", { id: $("target").value });
        } catch (message) {
          showStatus(String(message), true);
        }
      });
    },
    show(view) {
      $("own-id").textContent = view.id ?? "Conectando…";
      const notice = $("notice");
      notice.hidden = !view.notice;
      notice.textContent = view.notice ?? "";

      const busy = view.viewer.state === "asking" || view.viewer.state === "connected";
      $("connect-button").disabled = busy;
      switch (view.viewer.state) {
        case "asking":
          showStatus("Aguardando o outro computador aceitar…");
          break;
        case "connected":
          showStatus("Conectado. A tela abriu em outra janela.");
          break;
        case "failed":
          showStatus(view.viewer.message, true);
          break;
        default:
          showStatus("");
      }
    },
  },

  ask: {
    current: null,
    timer: null,
    start() {
      $("refuse").addEventListener("click", () => this.reply(null));
      $("accept").addEventListener("click", () =>
        this.reply({ control: $("control").checked }),
      );
    },
    reply(choice) {
      if (this.current !== null) {
        invoke("answer", { ask: this.current, choice });
      }
    },
    show(view) {
      if (!view.ask || view.ask.ask === this.current) {
        return;
      }
      this.current = view.ask.ask;
      $("name").textContent = view.ask.name;
      $("id").textContent = view.ask.id;
      $("control").checked = true;
      $("accept").disabled = true;
      setTimeout(() => {
        $("accept").disabled = false;
      }, ACCEPT_DELAY_MS);

      const deadline = Date.now() + view.ask.seconds * 1000;
      const tick = () => {
        const left = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
        $("countdown").textContent = `Sem resposta, será recusado em ${left} s.`;
      };
      tick();
      clearInterval(this.timer);
      this.timer = setInterval(tick, 1000);
    },
  },

  bar: {
    start() {
      $("end").addEventListener("click", () => invoke("end_session"));
    },
    show(view) {
      if (!view.session) {
        return;
      }
      const { name, id, permissions } = view.session;
      const who = id ? `${name} (ID ${id})` : name;
      const doing = permissions.control ? "vendo e controlando" : "vendo";
      $("session").textContent = `${who} está ${doing} este computador`;
      $("session").title = $("session").textContent;
    },
  },
};

function showStatus(text, error = false) {
  $("status").textContent = text;
  $("status").classList.toggle("error", error);
}

async function main() {
  const page = pages[document.body.dataset.page];
  page.start();
  const refresh = async () => page.show(await invoke("view"));
  await listen("changed", refresh);
  await refresh();
}

main();
