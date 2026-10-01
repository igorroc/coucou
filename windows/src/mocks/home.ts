// Placeholder data for the home dashboard. Everything here is a fixture to be
// swapped for a real source later (calendar, ticket tracker, prompt library).
// Sessions are NOT mocked — they come from State.sessions (hooks/opencode).

import { ICONS } from "../views/icons";

export const HOME_IDENTITY = {
  name: "Mochi",
  subtitle: "Pronto para ajudar.",
};

export interface MockMeeting {
  title: string;
  day: string;
  time: string;
  provider: string;
  url: string;
}

export const MOCK_MEETING: MockMeeting = {
  title: "Daily GatewayFy",
  day: "Hoje",
  time: "08:30",
  provider: "Google Meet",
  url: "https://meet.google.com/",
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
