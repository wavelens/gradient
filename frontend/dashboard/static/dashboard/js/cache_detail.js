/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

let statsCheckInterval;
let lastUpdateTime = 0;

// Get variables from global scope
const baseUrl = window.location.origin;
const cacheName = window.location.pathname.split('/')[2];

async function checkCacheStatus() {
  try {
    const response = await fetch(`${baseUrl}/api/v1/caches/${cacheName}/status`, {
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
        updateCacheData(data.message);
      }
    }
  } catch (error) {
    console.error("Error checking cache status:", error);
  }
}

function updateCacheData(cacheData) {
  // Update quick stats
  if (cacheData.hit_rate !== undefined) {
    const hitRateElement = document.querySelector('.dashboard-stats .stat-card:nth-child(1) h3');
    if (hitRateElement) hitRateElement.textContent = `${cacheData.hit_rate}%`;
  }
  
  if (cacheData.storage_used !== undefined) {
    const storageElement = document.querySelector('.dashboard-stats .stat-card:nth-child(2) h3');
    if (storageElement) storageElement.textContent = cacheData.storage_used;
  }
  
  if (cacheData.avg_response_time !== undefined) {
    const responseTimeElement = document.querySelector('.dashboard-stats .stat-card:nth-child(3) h3');
    if (responseTimeElement) responseTimeElement.textContent = cacheData.avg_response_time;
  }
  
  if (cacheData.uptime !== undefined) {
    const uptimeElement = document.querySelector('.dashboard-stats .stat-card:nth-child(4) h3');
    if (uptimeElement) uptimeElement.textContent = cacheData.uptime;
  }
  
  // Update performance metrics
  if (cacheData.cache_hits !== undefined) {
    const hitsElement = document.querySelector('.item-meta .text-light-n-s');
    if (hitsElement && hitsElement.textContent.includes('hits')) {
      hitsElement.textContent = `${cacheData.cache_hits} hits`;
    }
  }
  
  // Update recent activity if provided
  if (cacheData.recent_activity) {
    updateRecentActivity(cacheData.recent_activity);
  }
  
  // Update action buttons based on status
  updateActionButtons(cacheData.status);
}

function updateRecentActivity(activities) {
  const activityContainer = document.getElementById('recent-activity');
  if (!activityContainer || !activities) return;
  
  // Clear current content
  activityContainer.innerHTML = '';
  
  if (activities.length === 0) {
    activityContainer.innerHTML = `
      <div class="no-caches empty-state">
        <span class="material-symbols-outlined empty-state-icon">hub</span>
        <h3>No recent activity</h3>
        <p>Cache activity will appear here when the cache is in use.</p>
      </div>
    `;
    return;
  }
  
  activities.slice(0, 5).forEach(activity => {
    const iconMap = {
      'Cache Hit': 'check_circle',
      'Cache Miss': 'search_off',
      'Cache Eviction': 'delete',
      'Cache Store': 'save'
    };
    
    const activityElement = document.createElement('div');
    activityElement.className = 'workflow-item dashboard-item';
    activityElement.innerHTML = `
      <div class="full-width">
        <div class="item-header">
          <span class="material-symbols-outlined item-icon">${iconMap[activity.action] || 'hub'}</span>
          <span class="workflow-title">${activity.action}</span>
        </div>
        <p class="text-light">
          Key: <code>${activity.key}</code>
          • ${activity.timestamp}
          ${activity.response_time !== 'N/A' ? `• ${activity.response_time}` : ''}
        </p>
      </div>
    `;
    activityContainer.appendChild(activityElement);
  });
}

function updateActionButtons(status) {
  const actionsContainer = document.querySelector('.project-actions');
  if (!actionsContainer) return;
  
  // Update the toggle button based on current status
  const toggleButton = actionsContainer.querySelector('button[onclick*="toggleCache"]');
  if (toggleButton) {
    if (status === 'active') {
      toggleButton.className = 'submit-btn danger-btn';
      toggleButton.onclick = () => toggleCache(cacheName, false);
      toggleButton.innerHTML = `
        <span class="material-symbols-outlined">stop</span>
        Deactivate Cache
      `;
    } else {
      toggleButton.className = 'submit-btn';
      toggleButton.onclick = () => toggleCache(cacheName, true);
      toggleButton.innerHTML = `
        <span class="material-symbols-outlined">play_arrow</span>
        Activate Cache
      `;
    }
  }
}

function formatTimeAgo(dateString) {
  if (!dateString) return 'just now';
  
  const date = new Date(dateString);
  const now = new Date();
  const diffMs = now - date;
  const diffMins = Math.floor(diffMs / 60000);
  const diffHours = Math.floor(diffMins / 60);
  const diffDays = Math.floor(diffHours / 24);
  
  if (diffDays > 0) {
    return `${diffDays} day${diffDays > 1 ? 's' : ''} ago`;
  } else if (diffHours > 0) {
    return `${diffHours} hour${diffHours > 1 ? 's' : ''} ago`;
  } else if (diffMins > 0) {
    return `${diffMins} minute${diffMins > 1 ? 's' : ''} ago`;
  } else {
    return 'just now';
  }
}

// Check if cache is active and should be monitored
function shouldStartPolling() {
  const statusElements = document.querySelectorAll('.text-light-n-s');
  for (let element of statusElements) {
    const text = element.textContent.toLowerCase();
    if (text.includes('status: active')) {
      return true;
    }
  }
  return false;
}

// Initialize polling for active caches
if (shouldStartPolling()) {
  console.log('Starting cache monitoring for active cache');
  
  // Start status polling for active caches
  statsCheckInterval = setInterval(checkCacheStatus, 10000); // Check every 10 seconds
  
  // Auto-stop polling after 30 minutes to prevent endless polling
  setTimeout(() => {
    if (statsCheckInterval) {
      clearInterval(statsCheckInterval);
    }
    console.log('Auto-stopped cache polling after 30 minutes');
  }, 30 * 60 * 1000);
} else {
  console.log('Cache is inactive, monitoring not started');
}
