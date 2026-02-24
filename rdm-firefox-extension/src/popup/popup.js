// src/popup/popup.js

class VideoPopup {
    run() {
        if (document.readyState === 'loading') {
            document.addEventListener('DOMContentLoaded', () => this._init());
        } else {
            this._init();
        }
    }

    _init() {
        // Request current state from the background page.
        browser.runtime.sendMessage({ type: 'stat' }).then(resp => {
            this._render(resp);
        }).catch(err => {
            console.error('[rdm popup] stat error:', err);
        });

        // Monitoring toggle — wait for background to acknowledge before closing
        // so the message is guaranteed to be delivered to the event page.
        document.getElementById('chk-monitoring').addEventListener('change', e => {
            browser.runtime.sendMessage({ type: 'cmd', enabled: e.target.checked })
                .finally(() => window.close());
        });

        // Clear video list
        document.getElementById('btn-clear').addEventListener('click', () => {
            browser.runtime.sendMessage({ type: 'clear' });
            window.close();
        });

        // "More formats" hint
        document.getElementById('btn-more-formats').addEventListener('click', () => {
            alert('Play the video in the desired format in the web player first.');
        });
    }

    _render(response) {
        if (!response) return;

        const chk = document.getElementById('chk-monitoring');
        chk.checked = response.enabled;

        const list = response.list ?? [];

        if (list.length > 0) {
            document.getElementById('video-section').hidden = false;
            document.getElementById('empty-state').hidden   = true;
            this._renderVideoList(list);
        } else {
            document.getElementById('video-section').hidden = true;
            document.getElementById('empty-state').hidden   = false;
        }
    }

    _renderVideoList(items) {
        const container = document.getElementById('video-list');
        container.innerHTML = '';

        items.forEach(item => {
            const el = document.createElement('div');
            el.className = 'video-item';

            const btn = document.createElement('button');
            btn.className   = 'video-title';
            btn.dataset.id  = item.id;
            btn.textContent = item.text ?? '(unknown)';

            const info = document.createElement('span');
            info.className   = 'video-info';
            info.textContent = item.info ?? '';

            btn.addEventListener('click', e => {
                browser.runtime.sendMessage({ type: 'vid', itemId: e.currentTarget.dataset.id });
                window.close();
            });

            el.appendChild(btn);
            el.appendChild(info);
            container.appendChild(el);
        });
    }
}

new VideoPopup().run();
