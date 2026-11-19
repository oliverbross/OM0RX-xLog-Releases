import { showHMError } from './feedback.js';
// Keep a failed GPU/module load recoverable without a persistent error banner.
try {
  await import('./dashboard.js');
} catch (error) {
  const status = document.getElementById('xlog-link');
  status.textContent = '';
  showHMError('HM Control could not start. Check graphics support and reload this window.', 'startup');
  const retry = document.getElementById('xlog-retry');
  retry.hidden = false; retry.textContent = 'Reload HM Control';
  retry.addEventListener('click', () => location.reload());
  console.error('HM Control initialization failed', error);
}
