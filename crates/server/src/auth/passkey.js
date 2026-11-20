"use strict";

const form = document.getElementById("consent");
const allow = document.getElementById("allow");
const status = document.getElementById("status");
const registeredAccount = document.getElementById("account");
const token = document.getElementById("token");
let confirmedOptions;

allow.addEventListener("click", async () => {
    allow.disabled = true;
    status.textContent = "";
    try {
        let options = confirmedOptions;
        if (!options) {
            const response = await fetch("/passkey/begin", {
                method: "POST",
                headers: {"Content-Type": "application/json"},
                body: JSON.stringify({request: form.elements.request.value, token: token?.value ?? ""}),
            });
            const result = await response.json();
            if (!response.ok) {
                throw new Error(result.error);
            }
            if (result.account) {
                const {name, user_id} = result.account;
                registeredAccount.textContent = `Create a passkey for ${name || user_id} (@${user_id}).`;
                token.value = "";
                token.disabled = true;
                confirmedOptions = result.public_key;
                allow.textContent = "Create passkey and allow access";
                return;
            }
            options = result.public_key;
        }
        const registering = form.dataset.action === "register";
        const publicKey = registering
            ? PublicKeyCredential.parseCreationOptionsFromJSON(options)
            : PublicKeyCredential.parseRequestOptionsFromJSON(options);
        const credential = registering
            ? await navigator.credentials.create({publicKey})
            : await navigator.credentials.get({publicKey});
        if (!credential) {
            throw new Error("No passkey was selected.");
        }
        form.elements.credential.value = JSON.stringify(credential.toJSON());
        form.requestSubmit(document.getElementById("finish"));
    } catch (error) {
        status.textContent = error instanceof Error ? error.message : "Passkey sign-in failed.";
    } finally {
        allow.disabled = false;
    }
});
