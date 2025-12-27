/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

let statusCheckInterval;
let lastUpdateTime = 0;

// Get variables from global scope
window.baseUrl = window.baseUrl || window.location.origin;
const orgName = window.location.pathname.split('/')[2];
const projectName = window.location.pathname.split('/')[4];

async function checkProjectStatus() {
  try {
    const response = await fetch(`${window.baseUrl}/api/v1/projects/${orgName}/${projectName}/status`, {
      method: "GET",
      credentials: "include",
      withCredentials: true,
      mode: "cors",
      headers: {
        "Authorization": `Bearer ${window.token || ''}`,
        "Content-Type": "application/jsonstream",
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
      },
    });
    
    if (response.ok) {
      const data = await response.json();
      if (!data.error) {
        updateProjectData(data.message);
        
        // Check if any evaluations are running
        const hasRunningEvaluations = data.message.evaluations && 
          data.message.evaluations.some(eval => 
            eval.status === 'Running' || eval.status === 'Evaluating' || 
            eval.status === 'Building' || eval.status === 'Queued'
          );
        
        // Stop polling if no evaluations are running
        if (!hasRunningEvaluations) {
          clearInterval(statusCheckInterval);
          return;
        }
      }
    }
  } catch (error) {
    console.error("Error checking project status:", error);
  }
}

function updateProjectData(projectData) {
  // Update quick stats
  const totalRuns = document.querySelector('.dashboard-stats .stat-card:nth-child(1) h3');
  const successfulRuns = document.querySelector('.dashboard-stats .stat-card:nth-child(2) h3');
  const failedRuns = document.querySelector('.dashboard-stats .stat-card:nth-child(3) h3');
  const runningRuns = document.querySelector('.dashboard-stats .stat-card:nth-child(4) h3');
  
  if (totalRuns && projectData.evaluations) {
    totalRuns.textContent = projectData.evaluations.length;
  }
  
  if (projectData.evaluations) {
    const successful = projectData.evaluations.filter(e => e.status === 'Completed').length;
    const failed = projectData.evaluations.filter(e => e.status === 'Failed' || e.status === 'Aborted').length;
    const running = projectData.evaluations.filter(e => 
      e.status === 'Running' || e.status === 'Building' || 
      e.status === 'Evaluating' || e.status === 'Queued'
    ).length;
    
    if (successfulRuns) successfulRuns.textContent = successful;
    if (failedRuns) failedRuns.textContent = failed;
    if (runningRuns) runningRuns.textContent = running;
  }
  
  // Update recent evaluations section
  updateRecentEvaluations(projectData.evaluations);
  
  // Update action buttons
  updateActionButtons(projectData.evaluations);
}

function updateRecentEvaluations(evaluations) {
  const evaluationsContainer = document.querySelector('.dashboard-section:nth-child(2) .section-content');
  if (!evaluationsContainer || !evaluations) return;
  
  // Clear current content
  evaluationsContainer.innerHTML = '';
  
  const recentEvaluations = evaluations.slice(0, 5);
  
  if (recentEvaluations.length === 0) {
    evaluationsContainer.innerHTML = `
      <div class="no-caches empty-state">
        <span class="material-symbols-outlined empty-state-icon">science</span>
        <h3>No evaluations yet</h3>
        <p>Start your first evaluation to see results here.</p>
        <button type="button" class="submit-btn empty-state-link" onclick="startEvaluation('${orgName}', '${projectName}')">
          <span class="material-symbols-outlined">play_arrow</span>
          Start First Evaluation
        </button>
      </div>
    `;
    return;
  }
  
  recentEvaluations.forEach(evaluation => {
    const statusIcon = getStatusIcon(evaluation.status);
    const statusText = getStatusText(evaluation.status);
    const timeAgo = formatTimeAgo(evaluation.created_at);
    
    const evaluationElement = document.createElement('div');
    evaluationElement.className = 'workflow-item dashboard-item';
    evaluationElement.innerHTML = `
      <a href="/organization/${orgName}/log/${evaluation.id}" class="full-width normal-hover blue-span">
        <div class="item-header">
          <span class="material-symbols-outlined item-icon">science</span>
          <span class="workflow-title">Evaluation #${evaluation.id}</span>
        </div>
        <p class="text-light">
          <span class="material-symbols-outlined">${statusIcon}</span>
          ${statusText}
          • ${timeAgo} ago
        </p>
        <div class="item-meta">
          <span class="flex-c">
            <span class="material-symbols-outlined m-r-3">folder</span>
            <span class="text-light-n-s">${evaluation.total_builds || 0} builds</span>
          </span>
        </div>
      </a>
    `;
    evaluationsContainer.appendChild(evaluationElement);
  });
}

function updateActionButtons(evaluations) {
  const actionsContainer = document.querySelector('.project-actions');
  if (!actionsContainer) return;
  
  // Check if there are running evaluations
  const runningEvaluations = evaluations?.filter(e => 
    e.status === 'Running' || e.status === 'Building' || 
    e.status === 'Evaluating' || e.status === 'Queued'
  ) || [];
  
  // Remove existing abort buttons
  const existingAbortButtons = actionsContainer.querySelectorAll('.danger-btn');
  existingAbortButtons.forEach(btn => btn.remove());
  
  // Add abort buttons for running evaluations
  runningEvaluations.forEach(evaluation => {
    const abortButton = document.createElement('button');
    abortButton.type = 'button';
    abortButton.className = 'submit-btn danger-btn';
    abortButton.onclick = () => abortEvaluation(evaluation.id);
    abortButton.innerHTML = `
      <span class="material-symbols-outlined">stop</span>
      Abort Evaluation
    `;
    actionsContainer.appendChild(abortButton);
  });
}

function getStatusIcon(status) {
  switch (status?.toLowerCase()) {
    case 'running':
    case 'building':
    case 'evaluating':
      return 'hourglass_empty';
    case 'completed':
      return 'check_circle';
    case 'failed':
      return 'error';
    case 'aborted':
      return 'cancel';
    default:
      return 'schedule';
  }
}

function getStatusText(status) {
  switch (status?.toLowerCase()) {
    case 'running':
      return 'Running';
    case 'building':
      return 'Building';
    case 'evaluating':
      return 'Evaluating';
    case 'completed':
      return 'Completed';
    case 'failed':
      return 'Failed';
    case 'aborted':
      return 'Aborted';
    case 'queued':
      return 'Queued';
    default:
      return 'Pending';
  }
}

function formatTimeAgo(dateString) {
  if (!dateString) return '0 minutes';
  
  const date = new Date(dateString);
  const now = new Date();
  const diffMs = now - date;
  const diffMins = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMins / 60);
  const diffDays = Math.floor(diffHours / 24);
  
  if (diffDays > 0) {
    return `${diffDays} day${diffDays > 1 ? 's' : ''}`;
  } else if (diffHours > 0) {
    return `${diffHours} hour${diffHours > 1 ? 's' : ''}`;
  } else if (diffMins > 0) {
    return `${diffMins} minute${diffMins > 1 ? 's' : ''}`;
  } else {
    return 'just now';
  }
}

// Check if there are any running evaluations on page load
function hasRunningEvaluations() {
  const runningElements = document.querySelectorAll('.workflow-item .text-light');
  for (let element of runningElements) {
    const text = element.textContent.toLowerCase();
    if (text.includes('running') || text.includes('building') || 
        text.includes('evaluating') || text.includes('queued')) {
      return true;
    }
  }
  return false;
}

// Initialize polling if there are running evaluations
if (hasRunningEvaluations()) {
  statusCheckInterval = setInterval(checkProjectStatus, 3000); // Check every 3 seconds
  
  // Auto-stop polling after 30 minutes to prevent endless polling
  setTimeout(() => {
    if (statusCheckInterval) {
      clearInterval(statusCheckInterval);
    }
  }, 30 * 60 * 1000);
}
