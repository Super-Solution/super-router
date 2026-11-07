// Waitlist forms post to PUBLIC_WAITLIST_URL when it is configured.
// Without it there is nowhere to store the address, so the form says so
// instead of pretending the sign-up worked.

const endpoint = import.meta.env.PUBLIC_WAITLIST_URL as string | undefined;

for (const form of document.querySelectorAll<HTMLFormElement>('form[data-waitlist]')) {
  const status = form.querySelector<HTMLElement>('[data-waitlist-status]');
  const button = form.querySelector<HTMLButtonElement>('button[type="submit"]');

  const say = (message: string, tone: 'ok' | 'error' | 'info') => {
    if (!status) return;
    status.textContent = message;
    status.dataset.tone = tone;
  };

  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    const email = new FormData(form).get('email');
    if (typeof email !== 'string' || !form.checkValidity()) {
      say('Enter a valid email address, like name@example.com.', 'error');
      return;
    }
    if (!endpoint) {
      say('Sign-ups open when SuperRouter Cloud launches. The waitlist isn’t collecting addresses yet.', 'info');
      return;
    }
    button?.setAttribute('disabled', '');
    say('Adding you…', 'info');
    try {
      const response = await fetch(endpoint, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ email, source: form.dataset.waitlist }),
      });
      if (!response.ok) throw new Error(String(response.status));
      form.reset();
      say('You’re on the list. We’ll email you when Cloud opens.', 'ok');
    } catch {
      say('That didn’t go through. Check your connection and try again.', 'error');
    } finally {
      button?.removeAttribute('disabled');
    }
  });
}
