// Placeholder data for the home dashboard. Only the prompt library is still a
// fixture: sessions, tasks (Jira) and the next appointment (Google Calendar)
// come from real sources.

import { ICONS } from "../views/icons";

export const HOME_IDENTITY = {
  name: "Mochi",
  subtitle: "Pronto para ajudar.",
};

export interface MockSuggestion {
  id: string;
  icon: string;
  label: string;
  /** What gets sent to the chat when the suggestion is clicked. */
  prompt: string;
}

export const MOCK_SUGGESTIONS: MockSuggestion[] = [
  {
    id: "sprint",
    icon: ICONS.clipboard,
    label: "Gerar relatório do sprint",
    prompt: "Gere um relatório do sprint atual.",
  },
  {
    id: "assigned",
    icon: ICONS.list,
    label: "Resumir tarefas atribuídas",
    prompt: "Resuma as tarefas que estão atribuídas a mim.",
  },
  {
    id: "daily",
    icon: ICONS.calendar,
    label: "Preparar daily",
    prompt: "Prepare a daily: o que fiz, o que vou fazer e bloqueios.",
  },
  {
    id: "mytasks",
    icon: ICONS.checkCircle,
    label: "Abrir minhas tarefas",
    prompt: "Liste minhas tarefas abertas e priorize.",
  },
];
