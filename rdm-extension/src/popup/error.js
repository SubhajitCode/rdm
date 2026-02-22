// src/popup/error.js
// Shown when the rdm native server is unreachable.

document.getElementById('btn-launch').addEventListener('click', () => {
    // Attempt to wake rdm via its custom URI scheme.
    // This requires rdm to have registered "rdm+app://" with the OS.
    window.open('rdm+app://launch');
    window.close();
});
