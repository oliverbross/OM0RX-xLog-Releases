import '../src/live/AppDialog.css';
import '../src/live/FeedbackDialog.css';

const dismissed = new Set();
let dialog, message, current, previous;
export function showHMError(text, key = text) {
  if (!text || dismissed.has(key)) return;
  if (!dialog?.isConnected) {
    dialog = document.createElement('dialog');
    dialog.className = 'xlog-dialog feedback-dialog';
    dialog.setAttribute('role', 'alertdialog'); dialog.setAttribute('aria-label', 'HM Control');
    const header = document.createElement('header'), title = document.createElement('h2'); title.textContent = 'HM Control';
    const close = () => { dismissed.add(current); dialog.close(); previous?.focus({ preventScroll: true }); };
    const cross = document.createElement('button'); cross.type = 'button'; cross.textContent = '×'; cross.setAttribute('aria-label', 'Close dialog'); cross.onclick = close;
    header.append(title, cross);
    const body = document.createElement('div'); body.className = 'xlog-dialog-content';
    message = document.createElement('p'); message.className = 'feedback-message';
    const actions = document.createElement('div'); actions.className = 'dialog-actions';
    const button = document.createElement('button'); button.type = 'button'; button.textContent = 'Close'; button.onclick = close;
    actions.append(button); body.append(message, actions); dialog.append(header, body); document.body.append(dialog);
    dialog.addEventListener('cancel', event => { event.preventDefault(); close(); });
  }
  current = key; message.textContent = String(text);
  if (!dialog.open) { previous = document.activeElement; dialog.showModal(); dialog.querySelector('.dialog-actions button').focus({ preventScroll: true }); }
}
