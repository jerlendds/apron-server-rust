"use strict";

const form = document.getElementById("login");
const status = document.getElementById("status");
const showToken = document.getElementById("show-token");
const output = document.getElementById("resume-token");
const params = new URLSearchParams(location.hash.slice(1));
const server = new URL("/ws", location.href);
server.protocol = location.protocol === "https:" ? "wss:" : "ws:";
const token = params.get("token");
const state = params.get("state");
let resumeToken;

if (!token || params.get("server") !== server.href) {
    status.textContent = "Open the sign-in link sent by this Apron server.";
    form.hidden = true;
}

showToken.addEventListener("click", () => {
    output.value = resumeToken;
    output.parentElement.hidden = false;
    output.select();
});

form.addEventListener("submit", async event => {
    event.preventDefault();
    const submit = form.querySelector("button");
    submit.disabled = true;
    status.textContent = "Signing in…";
    const socket = new WebSocket(server);
    let sequence = 0;
    const waiting = new Map();
    const connected = new Promise((resolve, reject) => {
        socket.addEventListener("open", resolve, {once: true});
        socket.addEventListener("error", () => reject(new Error("Could not reach the Apron server.")), {once: true});
    });
    socket.addEventListener("message", event => {
        const frame = JSON.parse(event.data);
        const reply = waiting.get(frame.id);
        if (reply) {
            waiting.delete(frame.id);
            reply(frame);
        }
    });
    async function auth(credentials) {
        const id = String(++sequence);
        const reply = new Promise(resolve => waiting.set(id, resolve));
        socket.send(JSON.stringify({id, method: "auth", params: credentials}));
        const frame = await reply;
        if (!frame.result?.you) {
            throw new Error(frame.error?.message ?? "Sign-in was declined.");
        }
        return frame.result;
    }
    let timer;
    try {
        const operation = async () => {
            await connected;
            let current;
            const existing = form.elements.existing.value.trim();
            if (existing) current = await auth({scheme: "token", token: existing});
            const credentials = {scheme: "email", token};
            if (state !== null) credentials.state = state;
            const result = await auth(credentials);
            resumeToken = result.token ?? current?.token ?? (existing || undefined);
            status.textContent = `Signed in as ${result.you.name || result.you.user_id}.`;
            showToken.hidden = !resumeToken;
            form.hidden = true;
            history.replaceState(null, "", "/login");
        };
        await Promise.race([
            operation(),
            new Promise((_, reject) => {
                timer = setTimeout(() => reject(new Error("Sign-in timed out. Try again.")), 8000);
            }),
        ]);
    } catch (error) {
        status.textContent = error.message;
    } finally {
        clearTimeout(timer);
        socket.close();
        submit.disabled = false;
    }
});
