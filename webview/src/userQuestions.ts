import type { AgentThreadEvent } from "./types";

export type UserQuestionOption = { label: string; description: string };
export type UserQuestion = {
  id: string;
  header: string;
  question: string;
  options: UserQuestionOption[];
  multiSelect: boolean;
};

export function questionsFromEvent(event: AgentThreadEvent): UserQuestion[] {
  if (event.kind !== "tool_use") return [];
  const tool = (event.text ?? "").toLowerCase().replace(/[^a-z]/g, "");
  if (tool !== "askuserquestion" && tool !== "requestuserinput") return [];
  const data = record(event.data);
  const input = record(data?.input) ?? data;
  const rawQuestions = Array.isArray(input?.questions) ? input.questions : [];
  return rawQuestions.flatMap((raw, index) => {
    const question = record(raw);
    const text = string(question?.question);
    if (!text) return [];
    const options = Array.isArray(question?.options)
      ? question.options.flatMap((rawOption) => {
          const option = record(rawOption);
          const label = string(option?.label);
          return label
            ? [{ label, description: string(option?.description) }]
            : [];
        })
      : [];
    return [{
      id: string(question?.id) || `${event.id}-${index}`,
      header: string(question?.header) || `Question ${index + 1}`,
      question: text,
      options,
      multiSelect: question?.multiSelect === true || question?.multi_select === true,
    }];
  });
}

export function formatQuestionAnswers(
  questions: UserQuestion[],
  answers: Record<string, string[]>,
): string {
  return questions
    .map((question) => {
      const answer = answers[question.id]?.filter(Boolean).join(", ") ?? "";
      return questions.length === 1
        ? answer
        : `${question.header}: ${answer}`;
    })
    .join("\n");
}

function record(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object"
    ? (value as Record<string, unknown>)
    : null;
}

function string(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}
