let statusCheckInterval;
let logCheckInterval;
let buildCompleted = false;
let lastLogLength = 0;
let buildIds = [];

// Get variables from global scope
const baseUrl = window.location.origin;
const evaluationId = window.location.pathname.split('/').pop();

async function checkBuildStatus() {
  try {
    const response = await fetch(`${baseUrl}/api/evals/${evaluationId}/status`, {
      method: "GET",
      credentials: "include",
      headers: {
        "Content-Type": "application/json",
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
      },
    });
    
    if (response.ok) {
      const data = await response.json();
      if (!data.error) {
        const evaluation = data.message;
        updateBuildStatus(evaluation.status);
        
        // Display evaluation error if it exists
        displayEvaluationError(evaluation.error);
        
        // Get builds for this evaluation
        await fetchBuilds();
        
        // Stop polling if build is completed
        if (evaluation.status === 'Completed' || evaluation.status === 'Failed' || evaluation.status === 'Aborted') {
          clearInterval(statusCheckInterval);
          clearInterval(logCheckInterval);
          buildCompleted = true;
          return evaluation.status;
        }
      }
    }
  } catch (error) {
    console.error("Error checking build status:", error);
  }
  return null;
}

function updateBuildStatus(status) {
  const statusIcons = document.querySelectorAll('.status-icon');
  const statusTexts = document.querySelectorAll('.status-text');
  const abortButton = document.getElementById('abortButton');
  
  statusIcons.forEach(icon => {
    icon.className = 'material-icons status-icon';
    if (status === 'Completed') {
      icon.classList.add('green');
      icon.textContent = 'check_circle';
    } else if (status === 'Failed' || status === 'Aborted') {
      icon.classList.add('red');
      icon.textContent = 'cancel';
    } else {
      icon.className = 'loader status-icon';
    }
  });
  
  statusTexts.forEach(text => {
    text.textContent = status;
  });
  
  // Show/hide abort button based on status
  if (abortButton) {
    const showAbortButton = status === 'Building' || status === 'Evaluating' || status === 'Queued' || status === 'Running';
    abortButton.style.display = showAbortButton ? 'inline-block' : 'none';
  }
  
  // Update page title status indicator
  const titleStatusIcon = document.querySelector('.webkit-middle .material-icons, .webkit-middle .loader');
  if (titleStatusIcon) {
    if (status === 'Completed') {
      titleStatusIcon.className = 'material-icons center-image f-s-28px green';
      titleStatusIcon.textContent = 'check_circle';
    } else if (status === 'Failed' || status === 'Aborted') {
      titleStatusIcon.className = 'material-icons center-image f-s-28px red';
      titleStatusIcon.textContent = 'cancel';
    } else {
      titleStatusIcon.className = 'loader';
      titleStatusIcon.textContent = '';
    }
  }
}

function displayEvaluationError(error) {
  const logContainer = document.querySelector(".details-content");
  if (!logContainer) return;
  
  if (error) {
    // Clear existing content and show error
    logContainer.innerHTML = '';
    
    const errorDiv = document.createElement('div');
    errorDiv.className = 'line';
    errorDiv.style.cssText = 'color: #d32f2f; font-weight: bold; margin-bottom: 0.5rem;';
    errorDiv.textContent = '❌ Evaluation Error:';
    logContainer.appendChild(errorDiv);
    
    const errorContentDiv = document.createElement('div');
    errorContentDiv.className = 'line';
    errorContentDiv.style.cssText = 'color: #d32f2f; white-space: pre-wrap;';
    errorContentDiv.textContent = error;
    logContainer.appendChild(errorContentDiv);
    
    lastLogLength = 0; // Reset log counter since we cleared the container
  }
}

async function abortBuild() {
  try {
    const response = await fetch(`${baseUrl}/api/evals/${evaluationId}/abort`, {
      method: "POST",
      credentials: "include",
      headers: {
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
        "Content-Type": "application/json",
      }
    });
    
    if (response.ok) {
      const data = await response.json();
      if (!data.error) {
        // Force a status check to update UI
        await checkBuildStatus();
      } else {
        alert('Failed to abort build: ' + data.error);
      }
    } else {
      const data = await response.json().catch(() => ({}));
      alert('Failed to abort build: ' + (data.error || 'Unknown error'));
    }
  } catch (error) {
    console.error("Error aborting build:", error);
    alert('Error aborting build: ' + error.message);
  }
}

async function fetchBuilds() {
  try {
    const response = await fetch(`${baseUrl}/api/evals/${evaluationId}/builds`, {
      method: "GET",
      credentials: "include",
      headers: {
        "Content-Type": "application/json",
        "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
      },
    });
    
    if (response.ok) {
      const data = await response.json();
      if (!data.error) {
        buildIds = data.message.map(build => build.id);
        await updateLogs();
      }
    }
  } catch (error) {
    console.error("Error fetching builds:", error);
  }
}

async function updateLogs() {
  const logContainer = document.querySelector(".details-content");
  if (!logContainer) return;
  
  let allLogs = [];
  
  for (const buildId of buildIds) {
    try {
      const response = await fetch(`${baseUrl}/api/builds/${buildId}`, {
        method: "GET",
        credentials: "include",
        headers: {
          "Content-Type": "application/json",
          "X-CSRFToken": document.querySelector('[name=csrfmiddlewaretoken]')?.value || '',
        },
      });
      
      if (response.ok) {
        const data = await response.json();
        if (!data.error && data.message.log) {
          const lines = data.message.log.split('\n');
          allLogs = allLogs.concat(lines);
        }
      }
    } catch (error) {
      console.error(`Error fetching build ${buildId}:`, error);
    }
  }
  
  // Only update if we have new content
  if (allLogs.length > lastLogLength) {
    const newLines = allLogs.slice(lastLogLength);
    newLines.forEach(line => {
      if (line.trim()) {
        const lineDiv = document.createElement('div');
        lineDiv.className = 'line';
        lineDiv.textContent = line; // Use textContent to prevent XSS
        logContainer.appendChild(lineDiv);
      }
    });
    lastLogLength = allLogs.length;
    logContainer.scrollTop = logContainer.scrollHeight;
  }
}

// Initialize the page
async function initializePage() {
  // Check if there's an initial evaluation error from the server
  if (window.initialEvaluationError) {
    displayEvaluationError(window.initialEvaluationError);
    return; // Don't fetch builds/logs if there's an error
  }
  
  await fetchBuilds();
  await updateLogs();
}

// Check if evaluation is in a running state and start polling accordingly
function shouldStartPolling() {
  const statusElements = document.querySelectorAll('.status-text');
  for (let element of statusElements) {
    const status = element.textContent.toLowerCase();
    if (status.includes('building') || status.includes('evaluating') || 
        status.includes('running') || status.includes('queued')) {
      return true;
    }
  }
  return false;
}

// Initialize page first
initializePage();

// Only start polling if evaluation is in a running state
if (shouldStartPolling()) {
  console.log('Starting live polling for evaluation updates');
  
  // Start status polling
  statusCheckInterval = setInterval(checkBuildStatus, 2000); // Check every 2 seconds

  // Start log polling for live updates
  logCheckInterval = setInterval(updateLogs, 3000); // Check logs every 3 seconds

  // Auto-stop polling after 30 minutes to prevent endless polling
  setTimeout(() => {
    if (statusCheckInterval) {
      clearInterval(statusCheckInterval);
    }
    if (logCheckInterval) {
      clearInterval(logCheckInterval);
    }
    console.log('Auto-stopped polling after 30 minutes');
  }, 30 * 60 * 1000);
} else {
  console.log('Evaluation is not running, live polling not started');
}
