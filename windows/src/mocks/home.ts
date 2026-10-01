// Fallback data for the home dashboard. Only the suggestion labels are still a
// fixture — shown until the assistant has generated its own from the master
// instruction. Sessions, tasks (Jira), the next appointment (Google Calendar)
// and the generated suggestions all come from real sources.

import { ICONS } from "../views/icons";

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
    label: "Relatório do sprint",
    prompt: "Gere um relatório do sprint atual.",
  },
  {
    id: "assigned",
    icon: ICONS.list,
    label: "Tarefas atribuídas",
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
    label: "Tarefas abertas",
    prompt: "Liste minhas tarefas abertas e priorize.",
  },
];
