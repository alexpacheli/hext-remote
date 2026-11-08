// The app's pages. What comes from the other computer (names, chat) only ever
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
      const editing = (on) => {
        $("rename-form").hidden = !on;
        $("name-line").hidden = on || !this.name;
        if (on) {
          $("new-name").value = this.name ?? "";
          $("new-name").focus();
        }
      };
      $("rename").addEventListener("click", () => editing(true));
      $("rename-cancel").addEventListener("click", () => editing(false));
      $("rename-form").addEventListener("submit", async (event) => {
        event.preventDefault();
        try {
          await invoke("rename", { name: $("new-name").value });
          editing(false);
        } catch (message) {
          showStatus(String(message), true);
        }
      });
      $("connect").addEventListener("submit", async (event) => {
        event.preventDefault();
        try {
          await invoke("connect", {
            id: $("target").value,
            password: $("password").value || null,
            remember: $("remember").checked,
          });
        } catch (message) {
          showStatus(String(message), true);
        } finally {
          $("password").value = "";
          $("remember").checked = false;
          $("remember-line").hidden = true;
        }
      });
      startAccount();
      // Only with a password is there something to remember it by.
      $("password").addEventListener("input", () => {
        $("remember-line").hidden = $("password").value === "";
      });
    },
    show(view) {
      $("own-id").textContent = view.id ?? "Conectando…";
      this.name = view.name;
      $("own-name").textContent = view.name ? `Nome: ${view.name}` : "";
      $("name-line").hidden = !view.name || !$("rename-form").hidden;
      const notice = $("notice");
      notice.hidden = !view.notice;
      notice.textContent = view.notice ?? "";

      showAccount(view.account);
      showRecent(view.recent, view.online);
      showSessions(view.viewers);
      showTechnicians(view.technicians);
      if (view.viewer_failure) {
        showStatus(view.viewer_failure, true);
      } else {
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
          files: $("files").checked,
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
      $("files").checked = false;
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
    timer: null,
    seen: null,
    start() {
      $("end").addEventListener("click", () => invoke("end_session"));
      $("send").addEventListener("click", () => invoke("send_files"));
      $("chat").addEventListener("click", () => invoke("open_host_chat"));
      document.body.addEventListener("mouseenter", () => this.unfold());
      document.body.addEventListener("mouseleave", () => this.foldIn(1500));
    },
    unfold() {
      clearTimeout(this.timer);
      if (document.body.classList.contains("folded")) {
        document.body.classList.remove("folded");
        invoke("fold_bar", { folded: false });
      }
    },
    foldIn(ms) {
      clearTimeout(this.timer);
      this.timer = setTimeout(() => {
        if (!document.body.matches(":hover")) {
          document.body.classList.add("folded");
          invoke("fold_bar", { folded: true });
        }
      }, ms);
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
        permissions.files && "arquivos",
        unattended && "entrou com a senha",
      ]
        .filter(Boolean)
        .join(" · ");
      $("who").textContent = who;
      $("doing").textContent = view.transfer ?? doing;
      $("send").hidden = !permissions.files;
      $("session").title = [`${who}: ${doing}`, view.transfer].filter(Boolean).join(" — ");
      // Who is there and what they can do are read first, in full.
      const shown = `${who}: ${doing} (${view.transfer_news})`;
      if (shown !== this.seen) {
        this.seen = shown;
        this.unfold();
        this.foldIn(6000);
      }
    },
  },

  chat: {
    source: "chat_view",
    start() {
      const send = () => {
        const text = $("text").value;
        if (text.trim()) {
          $("text").value = "";
          invoke("chat_send", { text });
        }
      };
      $("write").addEventListener("submit", (event) => {
        event.preventDefault();
        send();
      });
      $("text").addEventListener("keydown", (event) => {
        if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
          event.preventDefault();
          send();
        }
      });
    },
    show(chat) {
      const list = $("lines");
      const atEnd = list.scrollHeight - list.scrollTop - list.clientHeight < 40;
      $("peer").textContent = chat?.peer ?? "";
      list.replaceChildren(
        ...(chat?.lines ?? []).map(({ author, text }) => {
          const line = document.createElement("li");
          line.className = author;
          line.textContent = text;
          return line;
        }),
      );
      if (atEnd) {
        list.scrollTop = list.scrollHeight;
      }
      const live = chat?.live ?? false;
      $("over").hidden = live;
      $("text").disabled = !live;
      $("write").querySelector("button").disabled = !live;
    },
  },
};

/** What a computer open from here is doing. */
function sessionState({ asking, locked }) {
  if (locked) {
    return "bloqueado: peça para alguém acordar a tela";
  }
  return asking ? "aguardando aceitar…" : "conectado";
}

/** The computers open from here, each in its window. */
function showSessions(viewers) {
  $("sessions-section").hidden = viewers.length === 0;
  $("sessions").replaceChildren(
    ...viewers.map((viewer) => {
      const who = viewer.name ? `${viewer.name} (${viewer.id})` : viewer.id;
      const label = document.createElement("span");
      label.className = "who";
      label.textContent = `${who}: ${sessionState(viewer)}`;
      label.title = label.textContent;
      const show = document.createElement("button");
      show.type = "button";
      show.className = "link";
      show.textContent = "Mostrar";
      show.hidden = viewer.asking;
      show.addEventListener("click", () => invoke("viewer_focus", { session: viewer.session }));
      const close = document.createElement("button");
      close.type = "button";
      close.className = "link";
      close.textContent = viewer.asking ? "Cancelar" : "Encerrar";
      close.addEventListener("click", () => invoke("viewer_close", { session: viewer.session }));
      const item = document.createElement("li");
      item.append(label, show, close);
      return item;
    }),
  );
}

/** The technicians trusted here without the password, each with Revogar. */
function showTechnicians(technicians) {
  $("technicians-section").hidden = technicians.length === 0;
  $("technicians").replaceChildren(
    ...technicians.map((technician) => {
      const label = document.createElement("span");
      label.className = "who";
      const since = new Date(technician.since * 1000).toLocaleDateString();
      label.textContent = `${technician.name} (${technician.id}), desde ${since}`;
      label.title = label.textContent;
      const revoke = document.createElement("button");
      revoke.type = "button";
      revoke.className = "link";
      revoke.textContent = "Revogar";
      revoke.addEventListener("click", () =>
        invoke("revoke_technician", { key: technician.key }),
      );
      const item = document.createElement("li");
      item.append(label, revoke);
      return item;
    }),
  );
}

/**
 * The computers accessed; a click fills in the ID. `online`: the IDs online
 * (null: not known, and no dots).
 */
function showRecent(computers, online) {
  $("recent-section").hidden = computers.length === 0;
  $("recent").replaceChildren(
    ...computers.map(({ id, name }) => {
      const pick = document.createElement("button");
      pick.type = "button";
      pick.className = "pick";
      pick.textContent = name ? `${name} (${id})` : id;
      pick.title = pick.textContent;
      if (online) {
        const on = online.includes(id);
        const dot = document.createElement("span");
        dot.className = on ? "dot online" : "dot";
        dot.title = on ? "Online" : "Offline";
        pick.title = `${pick.textContent}: ${dot.title}`;
        pick.prepend(dot);
      }
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

/** The account section: which panel shows, and the steps between them. */
const account = {
  view: null,
  /** A panel the user is in, over the one the view calls for. */
  panel: null,
  /** The password typed when registering, for confirming the address. */
  password: "",
};

function accountPanel(name) {
  account.panel = name;
  for (const panel of document.querySelectorAll(".account-panel")) {
    panel.hidden = panel.id !== `account-${name ?? (account.view ? "in" : "out")}`;
  }
  const first = !name ? null : document.querySelector(`#account-${name} input:not([hidden])`);
  first?.focus();
}

function accountStatus(text, error = false) {
  $("account-status").textContent = text;
  $("account-status").classList.toggle("error", error);
}

/** Makes an account call; the answer, or null after showing what went wrong. */
async function accountCall(call, busy) {
  accountStatus(busy);
  try {
    const reply = await invoke("account", { call });
    accountStatus("");
    return reply;
  } catch (message) {
    accountStatus(String(message), true);
    return null;
  }
}

function startAccount() {
  const on = (id, handler) => $(id).addEventListener("click", handler);
  const submit = (id, handler) =>
    $(id).addEventListener("submit", (event) => {
      event.preventDefault();
      handler();
    });
  for (const cancel of document.querySelectorAll(".account-panel .cancel")) {
    cancel.addEventListener("click", () => {
      account.password = "";
      accountStatus("");
      accountPanel(null);
    });
  }
  on("account-open-login", () => {
    $("login-second").hidden = true;
    $("login-second").value = "";
    accountPanel("login");
  });
  on("account-open-register", () => accountPanel("register"));
  on("account-open-forgot", () => {
    $("forgot-email").value = $("login-email").value;
    accountPanel("forgot");
  });
  on("account-open-totp", async () => {
    const reply = await accountCall({ call: "totp_setup" }, "Preparando…");
    if (reply?.reply !== "totp") return;
    $("totp-secret").textContent = reply.secret.replace(/(.{4})/g, "$1 ").trim();
    const svg = await invoke("account_qr", { text: reply.uri });
    // Our own SVG, made from the link: parsed as an image, not as markup.
    const image = new Image();
    image.alt = "QR code para o app autenticador";
    image.src = svg ? `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}` : "";
    $("totp-qr").replaceChildren(...(svg ? [image] : []));
    $("totp-code").value = "";
    accountPanel("totp-setup");
  });
  on("account-open-change", () => accountPanel("change"));
  on("account-logout", async () => {
    await accountCall({ call: "logout" }, "Saindo…");
    accountPanel(null);
  });
  on("account-recovery-done", () => {
    $("recovery-codes").replaceChildren();
    accountPanel(null);
  });

  submit("account-login", async () => {
    const second = $("login-second").value.trim();
    const reply = await accountCall(
      {
        call: "login",
        email: $("login-email").value,
        password: $("login-password").value,
        second: second || null,
      },
      "Entrando…",
    );
    if (reply?.reply === "needs_second_factor") {
      $("login-second").hidden = false;
      $("login-second").focus();
      accountStatus("Escreva o código do app autenticador.");
    } else if (reply) {
      $("login-password").value = "";
      $("login-second").value = "";
      accountPanel(null);
    }
  });
  submit("account-register", async () => {
    const password = $("register-password").value;
    if (password !== $("register-again").value) {
      accountStatus("As duas senhas não são iguais.", true);
      return;
    }
    const email = $("register-email").value;
    const reply = await accountCall({ call: "register", email, password }, "Criando…");
    if (!reply) return;
    account.password = password;
    $("register-password").value = "";
    $("register-again").value = "";
    $("verify-email").textContent = email.trim();
    $("verify-password").hidden = true;
    $("verify-code").value = "";
    accountPanel("verify");
  });
  submit("account-verify", async () => {
    const password = account.password || $("verify-password").value;
    if (!password) {
      $("verify-password").hidden = false;
      $("verify-password").focus();
      return;
    }
    const email = $("verify-email").textContent;
    const reply = await accountCall(
      { call: "verify", email, code: $("verify-code").value, password },
      "Confirmando…",
    );
    if (!reply) return;
    // Straight in, with the same password.
    const login = await accountCall({ call: "login", email, password }, "Entrando…");
    account.password = "";
    $("verify-password").value = "";
    if (login) {
      accountPanel(null);
      accountStatus("Conta criada. Ligue a verificação em duas etapas para ter computadores nela.");
    } else {
      $("login-email").value = email;
      accountPanel("login");
    }
  });
  submit("account-forgot", async () => {
    const reply = await accountCall(
      { call: "forgot_password", email: $("forgot-email").value },
      "Mandando…",
    );
    if (!reply) return;
    $("reset-code").value = "";
    accountPanel("reset");
  });
  submit("account-reset", async () => {
    const reply = await accountCall(
      {
        call: "reset_password",
        email: $("forgot-email").value,
        code: $("reset-code").value,
        password: $("reset-password").value,
      },
      "Trocando…",
    );
    if (!reply) return;
    $("reset-password").value = "";
    $("login-email").value = $("forgot-email").value;
    accountPanel("login");
    accountStatus("Senha trocada. Entre com a senha nova.");
  });
  submit("account-totp-setup", async () => {
    const reply = await accountCall(
      { call: "totp_enable", code: $("totp-code").value },
      "Ligando…",
    );
    if (reply?.reply !== "recovery_codes") return;
    $("recovery-codes").replaceChildren(
      ...reply.codes.map((code) => {
        const item = document.createElement("li");
        item.textContent = code;
        return item;
      }),
    );
    accountPanel("recovery");
  });
  submit("account-change", async () => {
    const reply = await accountCall(
      { call: "change_password", old: $("change-old").value, new: $("change-new").value },
      "Trocando…",
    );
    $("change-old").value = "";
    $("change-new").value = "";
    if (!reply) return;
    accountPanel(null);
    accountStatus("Senha trocada. Os outros computadores saíram da conta.");
  });
  accountPanel(null);
}

/** The account logged in here (null: none). */
function showAccount(view) {
  const was = account.view;
  account.view = view;
  if (view) {
    $("account-email").textContent = view.email;
    $("account-totp").textContent = view.totp
      ? "Verificação em duas etapas ligada."
      : "Verificação em duas etapas desligada: ela é obrigatória para ter computadores na conta.";
    $("account-open-totp").hidden = view.totp;
  }
  // In or out changed: back to what the view calls for.
  if (!!was !== !!view || account.panel === null) {
    accountPanel(null);
  }
}

function showStatus(text, error = false) {
  $("status").textContent = text;
  $("status").classList.toggle("error", error);
}

async function main() {
  const page = pages[document.body.dataset.page];
  page.start();
  const refresh = async () => page.show(await invoke(page.source ?? "view"));
  await listen("changed", refresh);
  await refresh();
}

main();
