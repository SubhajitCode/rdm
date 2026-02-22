// src/background/logger.js
export default class Logger {
    constructor() {
        this.enabled = true;
    }

    log(...args) {
        if (this.enabled) console.log('[rdm]', ...args);
    }

    warn(...args) {
        if (this.enabled) console.warn('[rdm]', ...args);
    }

    error(...args) {
        // errors always logged regardless of enabled flag
        console.error('[rdm]', ...args);
    }

    setEnabled(value) {
        this.enabled = !!value;
    }
}
