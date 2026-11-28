/** True for the chord that submits a multi-line field: Ctrl+Enter, or Cmd+Enter on a Mac. */
const isSubmitChord = (event: KeyboardEvent): boolean =>
  event.key === "Enter" && (event.ctrlKey || event.metaKey);

/** A form's submit handler: the page stays where it is and `send` does the work. */
export const onFormSubmit =
  (send: () => Promise<void>) =>
  (event: Event): void => {
    event.preventDefault();
    void send();
  };

/** A multi-line field's key handler: the submit chord sends, every other key types. */
export const onSubmitChord =
  (send: () => Promise<void>) =>
  (event: KeyboardEvent): void => {
    if (!isSubmitChord(event)) return;
    event.preventDefault();
    void send();
  };
