import test from 'node:test';
import assert from 'node:assert/strict';

import { buildOutline } from '../src/outline.js';

test('outline exposes child procedures inside exported object wrappers', () => {
  const source = `import { protectedProcedure } from "../trpc";

function calculateQualityStats(submissions: any[]) {
  return submissions.length;
}

export const projectQualityAnalyticsProcedures = {
  getQualityAnalytics: protectedProcedure
    .query(async () => {
      return calculateQualityStats([]);
    }),

  getQaApprovalStats: protectedProcedure
    .query(async () => {
      return 1;
    }),
};
`;

  const outline = buildOutline(source, 'quality-analytics.ts');

  assert.match(outline, /export const projectQualityAnalyticsProcedures = \{/);
  assert.match(outline, /getQualityAnalytics: protectedProcedure/);
  assert.match(outline, /getQaApprovalStats: protectedProcedure/);
  assert.match(outline, /\n\s*\d+│ \};/);
});

test('outline exposes child procedures inside call-expression wrappers like routers', () => {
  const source = `import { createTRPCRouter, protectedProcedure } from "../trpc";

export const projectRouter = createTRPCRouter({
  getStats: protectedProcedure
    .query(async () => {
      return 1;
    }),

  getSummary: protectedProcedure
    .query(async () => {
      return 2;
    }),
});
`;

  const outline = buildOutline(source, 'project.ts');

  assert.match(outline, /export const projectRouter = createTRPCRouter\(\{/);
  assert.match(outline, /getStats: protectedProcedure/);
  assert.match(outline, /getSummary: protectedProcedure/);
  assert.match(outline, /\n\s*\d+│ \}\);/);
});
