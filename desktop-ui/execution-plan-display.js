export class ExecutionPlanDisplay {
  constructor(container) {
    this.container = container;
    this.plan = null;
    this.currentStep = 0;
  }

  render(plan, currentStep = 0) {
    this.plan = plan;
    this.currentStep = currentStep;

    if (!plan || !plan.steps || plan.steps.length === 0) {
      this.container.innerHTML = '';
      return;
    }

    const html = `
      <div class="execution-plan">
        <div class="plan-header">
          <h4>执行计划</h4>
          <span class="plan-intent">${this.escape(plan.intent)}</span>
        </div>

        ${
          plan.explanation
            ? `
          <div class="plan-explanation">
            ${this.escape(plan.explanation)}
          </div>
        `
            : ''
        }

        ${
          plan.risks && plan.risks.length > 0
            ? `
          <div class="plan-risks">
            <strong>⚠️ 注意事项：</strong>
            <ul>
              ${plan.risks.map((risk) => `<li>${this.escape(risk)}</li>`).join('')}
            </ul>
          </div>
        `
            : ''
        }

        <div class="plan-steps">
          ${plan.steps.map((step, index) => this.renderStep(step, index)).join('')}
        </div>
      </div>
    `;

    this.container.innerHTML = html;
  }

  renderStep(step, index) {
    const status =
      index < this.currentStep ? 'completed' : index === this.currentStep ? 'active' : 'pending';

    const icon = status === 'completed' ? '✓' : status === 'active' ? '⟳' : '○';

    const operationIcon =
      {
        read: '📖',
        write: '✏️',
        delete: '🗑️',
        network: '🌐',
        analysis: '🔍',
      }[step.operationType] || '•';

    return `
      <div class="plan-step plan-step-${status}">
        <span class="step-icon">${icon}</span>
        <span class="step-number">${step.stepNumber}.</span>
        <span class="step-operation">${operationIcon}</span>
        <span class="step-description">${this.escape(step.description)}</span>
        ${!step.reversible ? '<span class="step-warning">⚠️ 不可逆</span>' : ''}
      </div>
    `;
  }

  escape(text) {
    const div = document.createElement('div');
    div.textContent = text;
    return div.innerHTML;
  }

  updateProgress(currentStep) {
    this.render(this.plan, currentStep);
  }
}
