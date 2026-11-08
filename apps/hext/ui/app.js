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
      showComputers(view.computers, view.online);
      showRecent(
        view.recent.filter((r) => !view.computers.some((c) => c.id === r.id)),
        view.online,
      );
      showSessions(view.viewers);
      showTechnicians(view.technicians);
      $("owners-section").hidden = !view.owners;
      $("owners-text").textContent = view.owners || "";
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
  on("devices-create", async () => {
    accountStatus("Criando a lista…");
    try {
      const code = await invoke("chain_create");
      accountStatus("");
      $("recovery-key").textContent = code;
      accountPanel("recovery-key");
    } catch (message) {
      accountStatus(String(message), true);
    }
  });
  on("account-recovery-key-done", () => {
    $("recovery-key").textContent = "";
    accountPanel(null);
    loadDevices();
  });
  on("devices-invite", async () => {
    const code = await invoke("chain_invite");
    $("invite-code").textContent = code;
    $("invite-state").textContent = "Esperando o outro computador…";
    accountPanel("invite");
    // Asks every few seconds whether it typed the code, for 10 minutes.
    const until = Date.now() + 10 * 60 * 1000;
    const ask = async () => {
      if (account.panel !== "invite" || $("invite-code").textContent !== code) return;
      if (Date.now() > until) {
        $("invite-state").textContent = "O código venceu. Feche e gere outro.";
        return;
      }
      try {
        const name = await invoke("chain_approve", { code });
        if (name) {
          $("invite-state").textContent = `${name} entrou na lista.`;
          loadDevices();
          return;
        }
      } catch (message) {
        $("invite-state").textContent = String(message);
      }
      setTimeout(ask, 5000);
    };
    setTimeout(ask, 5000);
  });
  on("owners-off", async () => {
    if (!confirm("Os dispositivos da conta voltam a precisar de aceite. Desligar?")) return;
    try {
      await invoke("account_unbind");
    } catch (message) {
      showStatus(String(message), true);
    }
  });
  on("computer-bind", async () => {
    if (!shownChain) return;
    if (
      !confirm(
        "Os dispositivos da lista da conta vão entrar neste computador sem ninguém aceitar. " +
          "Quem sair da lista deixa de entrar. Continuar?",
      )
    )
      return;
    try {
      await invoke("account_bind", { chain: shownChain });
      accountStatus(
        "Feito como administrador: os dispositivos da conta entram sem aceite. " +
          "Para desfazer: HextRemote service account --off.",
      );
    } catch (message) {
      accountStatus(String(message), true);
    }
  });
  on("computer-add", async () => {
    try {
      await invoke("computer_add");
      accountStatus("Este computador está em Meus computadores, nos dispositivos da conta.");
      loadDevices();
    } catch (message) {
      accountStatus(String(message), true);
    }
  });
  on("devices-open-join", () => {
    $("join-code").value = "";
    accountPanel("join");
  });
  submit("account-join", async () => {
    const typed = $("join-code").value;
    const recovery = typed.replace(/[^A-Za-z0-9]/g, "").length > 12;
    accountStatus(recovery ? "Entrando com a chave de recuperação…" : "Pedindo para entrar…");
    try {
      await invoke(recovery ? "chain_recover" : "chain_join", { code: typed });
      accountPanel(null);
      if (recovery) {
        accountStatus("Este computador entrou na lista.");
        loadDevices();
      } else {
        accountStatus("Pedido feito. Assim que o outro computador confirmar, este entra na lista.");
        // Looks again in a while: the other one approves within seconds.
        setTimeout(loadDevices, 8000);
        setTimeout(loadDevices, 20000);
      }
    } catch (message) {
      accountStatus(String(message), true);
    }
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

/** The account's devices, asked of the server (each signature checked). */
async function loadDevices() {
  if (!account.view) {
    $("devices").hidden = true;
    return;
  }
  let devices;
  try {
    devices = await invoke("account_devices");
  } catch (message) {
    $("devices").hidden = false;
    $("devices-hint").textContent = String(message);
    $("devices-list").replaceChildren();
    for (const id of ["devices-create", "devices-invite", "devices-open-join"]) $(id).hidden = true;
    return;
  }
  $("devices").hidden = false;
  const member = devices.state === "member";
  $("devices-hint").textContent = {
    none: account.view.totp
      ? "A conta ainda não tem a lista dos seus dispositivos. Comece por este computador."
      : "Ligue a verificação em duas etapas para colocar computadores na conta.",
    outside: "Este computador ainda não está na lista da conta.",
    member: "Os dispositivos da lista entram nos computadores da conta.",
  }[devices.state];
  $("devices-create").hidden = devices.state !== "none" || !account.view.totp;
  $("devices-invite").hidden = !member;
  $("devices-open-join").hidden = devices.state !== "outside";
  $("computer-add").hidden = !member || devices.listed;
  shownChain = devices.chain;
  $("chain-id").hidden = !member;
  $("chain-id").textContent = `ID da lista: ${devices.chain}`;
  $("computer-bind").hidden = !member || !devices.listed || !devices.bindable;
  $("computers-title").hidden = devices.computers.length === 0;
  $("shared-title").hidden = devices.shared.length === 0;
  $("shared-list").replaceChildren(
    ...devices.shared.map((computer) => {
      const label = document.createElement("span");
      label.className = "who";
      label.textContent = `${computer.name} (${computer.id}): ${allowedText(computer.allowed)}${untilText(computer.until)}`;
      label.title = label.textContent;
      const item = document.createElement("li");
      item.append(label);
      return item;
    }),
  );
  $("computers-list").replaceChildren(
    ...devices.computers.flatMap((computer) => {
      const label = document.createElement("span");
      label.className = "who";
      label.textContent = `${computer.name} (${computer.id})${computer.me ? ", este computador" : ""}`;
      label.title = label.textContent;
      const item = document.createElement("li");
      item.append(label);
      if (member) {
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "link";
        remove.textContent = "Remover";
        remove.addEventListener("click", async () => {
          if (!confirm(`Tirar ${computer.name} de Meus computadores?`)) return;
          try {
            await invoke("computer_remove", { host: computer.host });
            loadDevices();
          } catch (message) {
            accountStatus(String(message), true);
          }
        });
        const share = document.createElement("button");
        share.type = "button";
        share.className = "link";
        share.textContent = "Compartilhar";
        share.addEventListener("click", () => {
          sharing = sharing === computer.host ? null : computer.host;
          loadDevices();
        });
        item.append(share, remove);
      }
      return [item, ...shareRows(computer, member)];
    }),
  );
  $("devices-list").replaceChildren(
    ...devices.devices.map((device) => {
      const label = document.createElement("span");
      label.className = "who";
      label.textContent = device.me ? `${device.name} (este computador)` : device.name;
      label.title = label.textContent;
      const item = document.createElement("li");
      item.append(label);
      if (member && !device.me && devices.devices.length > 1) {
        const remove = document.createElement("button");
        remove.type = "button";
        remove.className = "link";
        remove.textContent = "Remover";
        remove.addEventListener("click", async () => {
          if (!confirm(`Tirar ${device.name} da conta? Ele deixa de entrar nos seus computadores.`)) {
            return;
          }
          try {
            await invoke("chain_remove", { key: device.key });
            loadDevices();
          } catch (message) {
            accountStatus(String(message), true);
          }
        });
        item.append(remove);
      }
      return item;
    }),
  );
}

/** The account logged in here (null: none). */
function showAccount(view) {
  const was = account.view;
  account.view = view;
  // Logged in, or 2FA turned on: the list again.
  if (!!was !== !!view || (view && was && view.totp !== was.totp)) {
    loadDevices();
  }
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

/** The computer whose share form is open (its host key), if any. */
let sharing = null;

/** What a share allows, in words. */
function allowedText(allowed) {
  const parts = ["ver"];
  if (allowed.control) parts.push("controlar");
  if (allowed.clipboard) parts.push("área de transferência");
  if (allowed.files) parts.push("arquivos");
  if (allowed.restart) parts.push("reiniciar");
  return parts.join(", ");
}

function untilText(until) {
  return until ? `, até ${new Date(until * 1000).toLocaleDateString()}` : "";
}

/** A computer's shares, each with Revogar, and the form to share it. */
function shareRows(computer, member) {
  const rows = computer.shares.map((share) => {
    const label = document.createElement("span");
    label.className = "who";
    label.textContent = `↳ ${share.name}: ${allowedText(share.allowed)}${untilText(share.until)}`;
    label.title = label.textContent;
    const item = document.createElement("li");
    item.className = "share";
    item.append(label);
    if (member) {
      const revoke = document.createElement("button");
      revoke.type = "button";
      revoke.className = "link";
      revoke.textContent = "Revogar";
      revoke.addEventListener("click", async () => {
        try {
          await invoke("share_remove", { host: computer.host, with: share.with });
          loadDevices();
        } catch (message) {
          accountStatus(String(message), true);
        }
      });
      item.append(revoke);
    }
    return item;
  });
  if (member && sharing === computer.host) rows.push(shareForm(computer));
  return rows;
}

function shareForm(computer) {
  const form = document.createElement("form");
  form.className = "share-form";
  form.innerHTML = `
    <label>ID da lista da outra conta (ela o vê em Dispositivos)
      <input name="with" required spellcheck="false" autocomplete="off"></label>
    <label>Nome, como a auditoria mostra (o e-mail, por exemplo)
      <input name="label" required maxlength="64" autocomplete="off"></label>
    <label><input type="checkbox" name="control" checked> Controlar</label>
    <label><input type="checkbox" name="clipboard"> Área de transferência</label>
    <label><input type="checkbox" name="files"> Arquivos</label>
    <label><input type="checkbox" name="restart"> Reiniciar</label>
    <label>Validade
      <select name="days">
        <option value="">Até revogar</option>
        <option value="1">1 dia</option>
        <option value="7">7 dias</option>
        <option value="30">30 dias</option>
      </select></label>
    <button type="submit" class="primary">Compartilhar</button>`;
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const field = (name) => form.elements.namedItem(name);
    try {
      await invoke("share_add", {
        host: computer.host,
        with: field("with").value.trim(),
        name: field("label").value.trim(),
        control: field("control").checked,
        clipboard: field("clipboard").checked,
        files: field("files").checked,
        restart: field("restart").checked,
        days: field("days").value ? Number(field("days").value) : null,
      });
      sharing = null;
      accountStatus(
        `${computer.name} compartilhado. Vale com o computador vinculado à conta (entrada sem aceite).`,
      );
      loadDevices();
    } catch (message) {
      accountStatus(String(message), true);
    }
  });
  const item = document.createElement("li");
  item.append(form);
  return item;
}

/** The list's ID, as last shown (for binding this computer to it). */
let shownChain = "";

/** The account's computers: a click connects. */
function showComputers(computers, online) {
  $("computers-section").hidden = computers.length === 0;
  $("computers").replaceChildren(
    ...computers.map(({ id, name }) => {
      const pick = document.createElement("button");
      pick.type = "button";
      pick.className = "pick";
      pick.textContent = name ? `${name} (${id})` : id;
      pick.title = `Conectar a ${pick.textContent}`;
      if (online) {
        const on = online.includes(id);
        const dot = document.createElement("span");
        dot.className = on ? "dot online" : "dot";
        dot.title = on ? "Online" : "Offline";
        pick.prepend(dot);
      }
      pick.addEventListener("click", async () => {
        try {
          await invoke("connect", { id, password: null, remember: false });
        } catch (message) {
          showStatus(String(message), true);
        }
      });
      const item = document.createElement("li");
      item.append(pick);
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
  const refresh = async () => page.show(await invoke(page.source ?? "view"));
  await listen("changed", refresh);
  await refresh();
}

main();
