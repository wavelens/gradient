async function makeRequest(){
    try {
        const response = await fetch(url, {
            method: "POST",
            credentials: 'include',
            withCredentials: true,
            mode: 'cors',
            headers: {
                "Authorization": `Bearer ${token}`,
                "Content-Type": "application/jsonstream",
            },
        });

        if (!response.ok){
            throw new Error('HTTP error! Status: ' + response.status);
        }

        const data = await response.text();
        if (data) {
            console.log("Response data:", data);
        } else {
            console.log("Die API hat keine Daten zurückgegeben.");
        }
    } catch (error) {
        console.error("Error during fetch:", error);
    }
}

makeRequest();

