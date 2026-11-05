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
          await invoke("connect", {
            id: $("target").value,
            password: $("password").value || null,
          });
        } catch (message) {
          showStatus(String(message), true);
        } finally {
          $("password").value = "";
        }
      });
    },
    show(view) {
      $("own-id").textContent = view.id ?? "Conectando…";
      const notice = $("notice");
      notice.hidden = !view.notice;
      notice.textContent = view.notice ?? "";

      showRecent(view.recent);
      const busy = view.viewer.state === "asking" || view.viewer.state === "connected";
      $("connect-button").disabled = busy;
      switch (view.viewer.state) {
        case "asking":
          showStatus(
            view.viewer.locked
              ? "O computador está bloqueado: peça para alguém acordar a tela."
              : "Aguardando o outro computador…",
          );
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
        this.reply({
          control: $("control").checked,
          clipboard: $("clipboard").checked,
          restart: $("restart").checked,
        }),
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
      $("clipboard").checked = false;
      $("restart").checked = false;
      $("restart").disabled = !view.ask.can_restart;
      $("restart-label").classList.toggle("soon", !view.ask.can_restart);
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
      const { name, id, permissions, unattended } = view.session;
      const who = id ? `${name} (ID ${id})` : name;
      const doing = [
        permissions.control ? "Vendo e controlando" : "Vendo",
        permissions.clipboard && "área de transferência",
        unattended && "entrou com a senha",
      ]
        .filter(Boolean)
        .join(" · ");
      $("who").textContent = who;
      $("doing").textContent = doing;
      $("session").title = `${who}: ${doing}`;
    },
  },
};

/** The computers accessed; a click fills in the ID. */
function showRecent(computers) {
  $("recent-section").hidden = computers.length === 0;
  $("recent").replaceChildren(
    ...computers.map(({ id, name }) => {
      const pick = document.createElement("button");
      pick.type = "button";
      pick.className = "pick";
      pick.textContent = name ? `${name} (${id})` : id;
      pick.title = pick.textContent;
      pick.addEventListener("click", () => {
        $("target").value = id;
        $("password").focus();
      });
      const forget = document.createElement("button");
      forget.type = "button";
      forget.className = "forget";
      forget.textContent = "×";
      forget.title = "Tirar da lista";
      forget.setAttribute("aria-label", `Tirar ${name || id} da lista`);
      forget.addEventListener("click", () => invoke("forget", { id }));
      const item = document.createElement("li");
      item.append(pick, forget);
      return item;
    }),
  );
}

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
